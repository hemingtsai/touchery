//! Execution guards shared by every Lua entry point in the app.
//!
//! Plugins and themes are user-authored scripts executed in-process, so each
//! call is bounded by an instruction budget and, as a backstop, by a wall-clock
//! deadline. The deadline is checked from a debug hook, and only the
//! interpreter delivers those: LuaJIT compiles hot loops into machine code
//! where no count hook fires, so a `while true do end` would otherwise run
//! forever. Turning the JIT compiler off first is what makes the budget real.

use mlua::{HookTriggers, Lua, VmState};
use std::cell::Cell;
use std::time::{Duration, Instant};

/// Instructions allowed for one Lua entry point. This is the primary bound:
/// with the JIT off it is machine-independent and cannot be evaded by a plain
/// loop.
const INSTRUCTION_BUDGET: u64 = 20_000_000;
/// How often the watchdog hook runs. Coarse enough to stay cheap, fine enough
/// that the wall-clock backstop below is honoured promptly.
const HOOK_INTERVAL: u32 = 10_000;
/// Backstop for machines on which the instruction budget alone would take an
/// unreasonable amount of time.
///
/// It is deliberately generous: a hook only runs between byte-code
/// instructions, so a plugin that legitimately blocks inside `io`/`os` calls
/// (which the README advertises) would otherwise be aborted right after the
/// blocking call returned. Neither limit can interrupt such a call — only
/// process isolation can — so this must not punish it either.
const WALL_CLOCK_BUDGET: Duration = Duration::from_secs(5);

/// Turn the JIT compiler off for `lua` and drop the `jit` global so scripts
/// cannot switch it back on. Byte-code then runs in the interpreter, where
/// the count hook is delivered.
pub fn disable_jit(lua: &Lua) {
    let _ = lua.load("if jit then jit.off() end").exec();
    let _ = lua.globals().set("jit", mlua::Value::Nil);
}

/// Run `f` under the shared budget.
///
/// The hook is installed as a *global* hook: in LuaJIT the hook mask lives in
/// the shared global state, so this also covers coroutines the script creates
/// while it runs. An overrun raises a normal Lua error, which the caller
/// reports like any other plugin/theme failure.
pub fn with_budget<T>(lua: &Lua, f: impl FnOnce() -> mlua::Result<T>) -> anyhow::Result<T> {
    let deadline = Instant::now() + WALL_CLOCK_BUDGET;
    let executed = Cell::new(0u64);

    lua.set_global_hook(
        HookTriggers {
            every_nth_instruction: Some(HOOK_INTERVAL),
            ..HookTriggers::new()
        },
        move |_, _| {
            let total = executed.get() + u64::from(HOOK_INTERVAL);
            executed.set(total);
            if total > INSTRUCTION_BUDGET {
                return Err(mlua::Error::RuntimeError(
                    "execution budget exceeded".into(),
                ));
            }
            if Instant::now() >= deadline {
                return Err(mlua::Error::RuntimeError(
                    "execution time limit exceeded".into(),
                ));
            }
            Ok(VmState::Continue)
        },
    )?;

    // Guard ensures the hook is removed on all exit paths, including panics.
    struct HookGuard<'a>(&'a Lua);
    impl Drop for HookGuard<'_> {
        fn drop(&mut self) {
            self.0.remove_global_hook();
            self.0.remove_hook();
        }
    }
    let _guard = HookGuard(lua);

    Ok(f()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget_error(run: impl Fn(&Lua) -> anyhow::Result<()>) -> String {
        let lua = Lua::new();
        disable_jit(&lua);
        let started = Instant::now();
        let error = run(&lua).expect_err("runaway script must be interrupted");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the budget must cut the script short"
        );
        error.to_string()
    }

    #[test]
    fn endless_loop_is_interrupted() {
        let error = budget_error(|lua| with_budget(lua, || lua.load("while true do end").exec()));
        assert!(
            error.contains("budget") || error.contains("time limit"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn coroutine_loop_is_interrupted() {
        let error = budget_error(|lua| {
            with_budget(lua, || {
                // `coroutine.resume` swallows the error and returns it, so
                // re-raise it to observe the interruption.
                lua.load(
                    "local co = coroutine.create(function() while true do end end)
                     assert(coroutine.resume(co))",
                )
                .exec()
            })
        });
        assert!(
            error.contains("budget") || error.contains("time limit"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn ordinary_calls_still_complete() {
        let lua = Lua::new();
        disable_jit(&lua);
        let sum: i64 = with_budget(&lua, || lua.load("return 1 + 1").eval()).unwrap();
        assert_eq!(sum, 2);

        // The hook must not leak out of the guarded call: this second call
        // would use the previous call's already-expired deadline otherwise.
        std::thread::sleep(Duration::from_millis(300));
        let sum: i64 = with_budget(&lua, || lua.load("return 21 * 2").eval()).unwrap();
        assert_eq!(sum, 42);
    }
}
