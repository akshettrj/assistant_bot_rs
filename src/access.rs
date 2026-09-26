//! Who may use which module.
//!
//! The rules, from the most to the least privileged:
//! - the owner and the sudo users can use every module, everywhere;
//! - a user listed in `telegram.allowed_users.<module>` can use that module in
//!   any chat;
//! - every member of a chat listed in `telegram.allowed_chats.<module>` can use
//!   that module in that chat;
//! - [`AccessPolicy::Public`] modules can be used by anyone.
//!
//! The allow lists are runtime settings: they can be changed from Telegram
//! (see [`crate::settings`]).

use std::collections::{BTreeSet, HashMap, HashSet};

use teloxide::types::{ChatId, UserId};

use crate::config::TelegramConfig;

/// Who a module is available to, before the per-module allow lists apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessPolicy {
    /// Anyone can use the module.
    Public,
    /// The owner, the sudo users and whoever the allow lists name.
    Restricted,
    /// Only the owner and the sudo users; the allow lists are ignored.
    SudoOnly,
    /// Only the owner. For modules that could grant themselves more access,
    /// such as the one editing the settings.
    OwnerOnly,
}

/// The resolved access rules of [`TelegramConfig`].
#[derive(Clone, Debug)]
pub struct AccessControl {
    owner_id: UserId,
    sudo_users: HashSet<UserId>,
    allowed_users: HashMap<String, HashSet<UserId>>,
    allowed_chats: HashMap<String, HashSet<ChatId>>,
}

impl AccessControl {
    pub fn from_config(config: &TelegramConfig) -> Self {
        Self {
            owner_id: config.owner_id,
            sudo_users: config.sudo_users_id.iter().copied().collect(),
            allowed_users: collect_sets(&config.allowed_users),
            allowed_chats: collect_sets(&config.allowed_chats),
        }
    }

    pub fn owner_id(&self) -> UserId {
        self.owner_id
    }

    /// Whether the user can use every module. The owner is always a sudo user.
    pub fn is_sudo(&self, user: UserId) -> bool {
        user == self.owner_id || self.sudo_users.contains(&user)
    }

    /// Whether `user`, writing in `chat`, may use the module `module_id`.
    ///
    /// Either side can be unknown, e.g. channel posts have no sender.
    pub fn can_use(
        &self,
        module_id: &str,
        policy: AccessPolicy,
        user: Option<UserId>,
        chat: Option<ChatId>,
    ) -> bool {
        let is_sudo = user.is_some_and(|user| self.is_sudo(user));

        match policy {
            AccessPolicy::Public => true,
            AccessPolicy::SudoOnly => is_sudo,
            AccessPolicy::OwnerOnly => user == Some(self.owner_id),
            AccessPolicy::Restricted => {
                is_sudo
                    || user.is_some_and(|user| contains(&self.allowed_users, module_id, &user))
                    || chat.is_some_and(|chat| contains(&self.allowed_chats, module_id, &chat))
            }
        }
    }

    /// Every user with more than public access.
    pub fn privileged_users(&self) -> BTreeSet<UserId> {
        std::iter::once(self.owner_id)
            .chain(self.sudo_users.iter().copied())
            .chain(self.allowed_users.values().flatten().copied())
            .collect()
    }

    /// Every chat with more than public access.
    pub fn privileged_chats(&self) -> BTreeSet<ChatId> {
        self.allowed_chats.values().flatten().copied().collect()
    }
}

fn collect_sets<T: Copy + Eq + std::hash::Hash>(
    map: &HashMap<String, Vec<T>>,
) -> HashMap<String, HashSet<T>> {
    map.iter()
        .map(|(module, ids)| (module.clone(), ids.iter().copied().collect()))
        .collect()
}

fn contains<T: Eq + std::hash::Hash>(
    map: &HashMap<String, HashSet<T>>,
    module_id: &str,
    id: &T,
) -> bool {
    map.get(module_id).is_some_and(|ids| ids.contains(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::config_from_toml;

    const OWNER: UserId = UserId(1);
    const SUDO: UserId = UserId(2);
    const NOTES_USER: UserId = UserId(3);
    const STRANGER: UserId = UserId(4);
    const NOTES_CHAT: ChatId = ChatId(-100);
    const OTHER_CHAT: ChatId = ChatId(-200);

    fn access() -> AccessControl {
        let config = config_from_toml(
            r#"
[telegram]
bot_token = "t"
error_logs_chat_id = -1
owner_id = 1
sudo_users_id = [2]
allowed_users = { notes = [3] }
allowed_chats = { notes = [-100] }
"#,
        );
        AccessControl::from_config(&config.telegram)
    }

    #[test]
    fn public_modules_are_open_to_everyone() {
        let access = access();
        assert!(access.can_use("x", AccessPolicy::Public, Some(STRANGER), None));
        assert!(access.can_use("x", AccessPolicy::Public, None, None));
    }

    #[test]
    fn owner_and_sudo_can_use_everything() {
        let access = access();
        for user in [OWNER, SUDO] {
            assert!(access.is_sudo(user));
            for policy in [AccessPolicy::Restricted, AccessPolicy::SudoOnly] {
                assert!(access.can_use("anything", policy, Some(user), Some(OTHER_CHAT)));
            }
        }
    }

    #[test]
    fn owner_only_excludes_sudo_users() {
        let access = access();
        assert!(access.can_use("settings", AccessPolicy::OwnerOnly, Some(OWNER), None));
        assert!(!access.can_use("settings", AccessPolicy::OwnerOnly, Some(SUDO), None));
        assert!(!access.can_use("settings", AccessPolicy::OwnerOnly, None, Some(NOTES_CHAT)));
    }

    #[test]
    fn allowed_users_only_get_their_modules() {
        let access = access();
        assert!(!access.is_sudo(NOTES_USER));
        assert!(access.can_use("notes", AccessPolicy::Restricted, Some(NOTES_USER), None));
        assert!(!access.can_use("other", AccessPolicy::Restricted, Some(NOTES_USER), None));
        assert!(!access.can_use("notes", AccessPolicy::SudoOnly, Some(NOTES_USER), None));
    }

    #[test]
    fn allowed_chats_open_modules_to_their_members() {
        let access = access();
        assert!(access.can_use(
            "notes",
            AccessPolicy::Restricted,
            Some(STRANGER),
            Some(NOTES_CHAT)
        ));
        assert!(access.can_use("notes", AccessPolicy::Restricted, None, Some(NOTES_CHAT)));
        assert!(!access.can_use(
            "notes",
            AccessPolicy::Restricted,
            Some(STRANGER),
            Some(OTHER_CHAT)
        ));
        assert!(!access.can_use(
            "notes",
            AccessPolicy::SudoOnly,
            Some(STRANGER),
            Some(NOTES_CHAT)
        ));
    }

    #[test]
    fn strangers_are_denied() {
        let access = access();
        assert!(!access.can_use("notes", AccessPolicy::Restricted, Some(STRANGER), None));
        assert!(!access.can_use("notes", AccessPolicy::Restricted, None, None));
    }

    #[test]
    fn privileged_ids_are_collected() {
        let access = access();
        assert_eq!(
            access.privileged_users(),
            BTreeSet::from([OWNER, SUDO, NOTES_USER])
        );
        assert_eq!(access.privileged_chats(), BTreeSet::from([NOTES_CHAT]));
    }
}
