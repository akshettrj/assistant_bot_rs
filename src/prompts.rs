//! Questions answered by the user's next message, shared by every module
//! (see [`botconf_telegram::prompts`]): a module asks with
//! `ctx.prompts.ask(ID, ...)` and reads the answer in its handler with
//! `ctx.prompts.answer::<Data>(ID, &msg)`. The registry routes answers to
//! the module that asked first.

pub use botconf_telegram::prompts::*;
