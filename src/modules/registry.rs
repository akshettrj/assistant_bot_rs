use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::Arc,
};

use teloxide::{
    dptree,
    types::{BotCommand, ChatId, Update, UserId},
};

use crate::{
    access::AccessPolicy,
    config::AssistantConfig,
    context::AppContext,
    modules::{Module, ModuleInfo, UpdateHandler},
    settings::Snapshot,
};

/// Errors caught while loading the modules or validating the configuration
/// against them.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistryError {
    #[error("invalid module id `{0}`: ids must be non-empty and snake_case")]
    InvalidModuleId(&'static str),

    #[error("module id `{0}` is registered more than once")]
    DuplicateModule(&'static str),

    #[error("command `{command}` is declared by both the `{first}` and `{second}` modules")]
    DuplicateCommand {
        command: String,
        first: &'static str,
        second: &'static str,
    },

    #[error("unknown module id `{id}` in `{key}` (known modules: {known})")]
    UnknownModule {
        id: String,
        key: &'static str,
        known: String,
    },

    #[error("the `{0}` module cannot be disabled")]
    CannotDisable(&'static str),
}

/// A module along with its (cached) metadata.
pub struct RegisteredModule {
    pub info: ModuleInfo,
    pub commands: Vec<BotCommand>,
    pub always_enabled: bool,
    module: Arc<dyn Module>,
}

impl fmt::Debug for RegisteredModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegisteredModule")
            .field("info", &self.info)
            .field("commands", &self.commands)
            .field("always_enabled", &self.always_enabled)
            .finish_non_exhaustive()
    }
}

impl RegisteredModule {
    pub(crate) fn new(module: Arc<dyn Module>) -> Self {
        Self {
            info: module.info(),
            commands: module.commands(),
            always_enabled: module.always_enabled(),
            module,
        }
    }
}

/// Every available module, in routing order. Whether a module is enabled is a
/// runtime setting (`modules.disabled`), checked for each update.
#[derive(Debug)]
pub struct ModuleRegistry {
    modules: Vec<RegisteredModule>,
}

impl ModuleRegistry {
    /// Checks that the modules' ids and commands do not clash.
    ///
    /// Commands must be unique across all modules, disabled ones included,
    /// since those can be enabled at runtime.
    pub fn new(available: Vec<Arc<dyn Module>>) -> Result<Self, RegistryError> {
        let modules: Vec<_> = available.into_iter().map(RegisteredModule::new).collect();

        let mut ids = HashSet::new();
        for module in &modules {
            let id = module.info.id;
            if !is_valid_id(id) {
                return Err(RegistryError::InvalidModuleId(id));
            }
            if !ids.insert(id) {
                return Err(RegistryError::DuplicateModule(id));
            }
        }

        check_commands(&modules)?;
        Ok(Self { modules })
    }

    pub fn iter(&self) -> impl Iterator<Item = &RegisteredModule> {
        self.modules.iter()
    }

    pub fn get(&self, id: &str) -> Option<&RegisteredModule> {
        self.iter().find(|module| module.info.id == id)
    }

    pub fn len(&self) -> usize {
        self.modules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }

    /// The modules enabled in `settings`.
    pub fn enabled<'a>(
        &'a self,
        settings: &'a Snapshot,
    ) -> impl Iterator<Item = &'a RegisteredModule> + 'a {
        self.iter()
            .filter(|module| settings.is_enabled(module.info.id))
    }

    /// The enabled modules that `user`, writing in `chat`, may use.
    pub fn accessible<'a>(
        &'a self,
        settings: &'a Snapshot,
        user: Option<UserId>,
        chat: Option<ChatId>,
    ) -> impl Iterator<Item = &'a RegisteredModule> + 'a {
        self.enabled(settings).filter(move |module| {
            settings
                .access
                .can_use(module.info.id, module.info.access, user, chat)
        })
    }

    /// Module ids in the config must exist, otherwise a typo would silently
    /// deny access (or fail to disable a module).
    pub fn validate_config(&self, config: &AssistantConfig) -> Result<(), RegistryError> {
        let references = [
            (
                "modules.disabled",
                config.modules.disabled.iter().collect::<Vec<_>>(),
            ),
            (
                "telegram.allowed_users",
                config.telegram.allowed_users.keys().collect(),
            ),
            (
                "telegram.allowed_chats",
                config.telegram.allowed_chats.keys().collect(),
            ),
        ];

        for (key, ids) in references {
            if let Some(id) = ids.into_iter().find(|id| self.get(id).is_none()) {
                let known: Vec<_> = self.iter().map(|module| module.info.id).collect();
                return Err(RegistryError::UnknownModule {
                    id: id.clone(),
                    key,
                    known: known.join(", "),
                });
            }
        }

        if let Some(module) = self.iter().find(|module| {
            module.always_enabled && config.modules.disabled.contains(module.info.id)
        }) {
            return Err(RegistryError::CannotDisable(module.info.id));
        }

        Ok(())
    }

    /// Settings that are valid but have no effect.
    pub fn lint_config(&self, config: &AssistantConfig) -> Vec<String> {
        let telegram = &config.telegram;
        self.iter()
            .filter(|module| {
                let id = module.info.id;
                let listed = telegram.allowed_users.contains_key(id)
                    || telegram.allowed_chats.contains_key(id);
                listed && module.info.access != AccessPolicy::Restricted
            })
            .map(|module| {
                format!(
                    "the allow lists of the `{}` module are ignored because of its {:?} access \
                     policy",
                    module.info.id, module.info.access
                )
            })
            .collect()
    }

    /// Routes every update to the first module that is enabled, allowed to
    /// and wants to handle it.
    pub fn handler(&self) -> UpdateHandler {
        self.modules
            .iter()
            .fold(dptree::entry(), |root, registered| {
                root.branch(access_gate(registered.info).chain(registered.module.handler()))
            })
    }
}

/// Lets an update through only if the module is enabled and its sender/chat
/// may use it, according to the current settings.
fn access_gate(info: ModuleInfo) -> UpdateHandler {
    dptree::filter(move |update: Update, ctx: Arc<AppContext>| {
        let settings = ctx.settings.current();
        if !settings.is_enabled(info.id) {
            return false;
        }

        let user = update.from().map(|user| user.id);
        let chat = update.chat().map(|chat| chat.id);
        let allowed = settings.access.can_use(info.id, info.access, user, chat);
        if !allowed {
            tracing::trace!(module = info.id, ?user, ?chat, "access denied");
        }
        allowed
    })
}

fn is_valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Two modules declaring the same command would make one of them unreachable.
fn check_commands(modules: &[RegisteredModule]) -> Result<(), RegistryError> {
    let mut owners = HashMap::new();
    for module in modules {
        for command in &module.commands {
            let name = command.command.trim_start_matches('/').to_lowercase();
            if let Some(first) = owners.insert(name.clone(), module.info.id) {
                return Err(RegistryError::DuplicateCommand {
                    command: format!("/{name}"),
                    first,
                    second: module.info.id,
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::ops::ControlFlow;

    use serde_json::json;
    use teloxide::{dptree::deps, utils::command::BotCommands};

    use super::*;
    use crate::test_support::{config_from_toml, context};

    const CONFIG: &str = r#"
[telegram]
bot_token = "t"
error_logs_chat_id = -1
owner_id = 1
allowed_users = { restricted = [3] }
"#;

    #[derive(BotCommands, Clone)]
    #[command(rename_rule = "lowercase")]
    enum PingCommand {
        #[command(description = "ping")]
        Ping,
    }

    /// A module handling every update it is allowed to see.
    pub(crate) struct TestModule {
        id: &'static str,
        access: AccessPolicy,
        commands: fn() -> Vec<BotCommand>,
        always_enabled: bool,
    }

    impl TestModule {
        pub(crate) fn arc(id: &'static str, access: AccessPolicy) -> Arc<dyn Module> {
            Arc::new(Self {
                id,
                access,
                commands: Vec::new,
                always_enabled: false,
            })
        }

        fn with_ping(id: &'static str) -> Arc<dyn Module> {
            Arc::new(Self {
                id,
                access: AccessPolicy::Public,
                commands: PingCommand::bot_commands,
                always_enabled: false,
            })
        }
    }

    impl Module for TestModule {
        fn info(&self) -> ModuleInfo {
            ModuleInfo {
                id: self.id,
                name: self.id,
                description: "test",
                access: self.access,
            }
        }

        fn commands(&self) -> Vec<BotCommand> {
            (self.commands)()
        }

        fn always_enabled(&self) -> bool {
            self.always_enabled
        }

        fn handler(&self) -> UpdateHandler {
            dptree::endpoint(|| async { Ok(()) })
        }
    }

    #[test]
    fn rejects_invalid_ids() {
        for id in ["", "Upper", "1st", "with-dash"] {
            let err =
                ModuleRegistry::new(vec![TestModule::arc(id, AccessPolicy::Public)]).unwrap_err();
            assert_eq!(err, RegistryError::InvalidModuleId(id));
        }
    }

    #[test]
    fn rejects_duplicate_ids() {
        let modules = vec![
            TestModule::arc("restricted", AccessPolicy::Public),
            TestModule::arc("restricted", AccessPolicy::Public),
        ];
        assert_eq!(
            ModuleRegistry::new(modules).unwrap_err(),
            RegistryError::DuplicateModule("restricted")
        );
    }

    #[test]
    fn rejects_duplicate_commands() {
        let modules = vec![TestModule::with_ping("a"), TestModule::with_ping("b")];
        assert_eq!(
            ModuleRegistry::new(modules).unwrap_err(),
            RegistryError::DuplicateCommand {
                command: "/ping".into(),
                first: "a",
                second: "b",
            }
        );
    }

    #[test]
    fn validates_module_references() {
        let registry =
            ModuleRegistry::new(vec![TestModule::arc("other", AccessPolicy::Public)]).unwrap();
        let err = registry
            .validate_config(&config_from_toml(CONFIG))
            .unwrap_err();
        assert!(
            matches!(&err, RegistryError::UnknownModule { id, key: "telegram.allowed_users", .. } if id == "restricted"),
            "{err}"
        );
    }

    #[test]
    fn always_enabled_modules_cannot_be_disabled() {
        let registry = ModuleRegistry::new(vec![
            TestModule::arc("restricted", AccessPolicy::Public),
            Arc::new(TestModule {
                id: "core",
                access: AccessPolicy::OwnerOnly,
                commands: Vec::new,
                always_enabled: true,
            }),
        ])
        .unwrap();
        let config = config_from_toml(&format!("{CONFIG}\n[modules]\ndisabled = [\"core\"]\n"));
        assert_eq!(
            registry.validate_config(&config).unwrap_err(),
            RegistryError::CannotDisable("core")
        );
    }

    #[test]
    fn lints_ignored_allow_lists() {
        let registry =
            ModuleRegistry::new(vec![TestModule::arc("restricted", AccessPolicy::SudoOnly)])
                .unwrap();
        let lints = registry.lint_config(&config_from_toml(CONFIG));
        assert_eq!(lints.len(), 1);
        assert!(lints[0].contains("`restricted`"), "{}", lints[0]);
    }

    #[tokio::test]
    async fn accessible_filters_by_access_and_enablement() {
        let ctx = context(
            CONFIG,
            vec![
                TestModule::arc("public", AccessPolicy::Public),
                TestModule::arc("restricted", AccessPolicy::Restricted),
                TestModule::arc("sudo", AccessPolicy::SudoOnly),
            ],
        )
        .await;

        let ids = |user: u64| -> Vec<_> {
            let settings = ctx.settings.current();
            ctx.modules
                .accessible(&settings, Some(UserId(user)), None)
                .map(|m| m.info.id)
                .collect()
        };
        assert_eq!(ids(1), ["public", "restricted", "sudo"]);
        assert_eq!(ids(3), ["public", "restricted"]);
        assert_eq!(ids(4), ["public"]);

        ctx.settings
            .set("modules.disabled", json!(["public"]), None, &ctx.modules)
            .await
            .unwrap();
        assert_eq!(ids(4), Vec::<&str>::new());
    }

    /// End-to-end check of the routing: updates only reach the modules their
    /// sender may use, and runtime changes apply to the next update.
    #[tokio::test]
    async fn handler_routes_through_access_gates() {
        let ctx = context(
            CONFIG,
            vec![TestModule::arc("restricted", AccessPolicy::Restricted)],
        )
        .await;
        let handler = ctx.modules.handler();

        let handled = |from: u64| {
            let update = message_update(from);
            let handler = handler.clone();
            let ctx = ctx.clone();
            async move {
                matches!(
                    handler.dispatch(deps![update, ctx]).await,
                    ControlFlow::Break(Ok(()))
                )
            }
        };

        assert!(handled(1).await, "owner is let through");
        assert!(handled(3).await, "allowed user is let through");
        assert!(!handled(4).await, "stranger is not");

        let registry = &ctx.modules;
        ctx.settings
            .add(
                "telegram.allowed_users.restricted",
                json!(4),
                None,
                registry,
            )
            .await
            .unwrap();
        assert!(handled(4).await, "newly allowed user is let through");

        ctx.settings
            .set("modules.disabled", json!(["restricted"]), None, registry)
            .await
            .unwrap();
        assert!(
            !handled(1).await,
            "disabled modules are skipped, even for the owner"
        );
    }

    fn message_update(from: u64) -> Update {
        let json = json!({
            "update_id": 1,
            "message": {
                "message_id": 1,
                "date": 0,
                "chat": { "id": from, "type": "private", "first_name": "Test" },
                "from": { "id": from, "is_bot": false, "first_name": "Test" },
                "text": "hello"
            }
        });
        // `Update`'s deserializer only works from a string: `from_value` yields
        // an `UpdateKind::Error`.
        let update: Update = serde_json::from_str(&json.to_string()).expect("valid update JSON");
        assert!(
            update.from().is_some(),
            "update parsed as {:?}",
            update.kind
        );
        update
    }
}
