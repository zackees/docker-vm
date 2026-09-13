set pagination off
set confirm off
set debuginfod enabled off
set auto-load off
set disable-randomization off
set follow-fork-mode parent
set detach-on-fork on
set print thread-events off
set print frame-arguments none
# A benign SIGCONT must not end batch `run` and make GDB kill the browser.
handle all nostop noprint pass
handle SIGABRT SIGSEGV SIGILL SIGBUS SIGFPE SIGSYS stop print pass
# Keep GDB's own startup/breakpoint traps from being delivered to the inferior.
handle SIGTRAP stop print nopass
run
echo \n=== Browser stopped: thread backtraces (no locals or arguments) ===\n
thread apply all bt 32
echo \n=== Shared library addresses ===\n
info sharedlibrary
echo \n=== Installed package versions ===\n
shell dpkg-query -W chromium libc6 gdb
