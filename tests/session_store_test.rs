use std::sync::Arc;

use tokio::sync::Mutex;

use winx_code_agent::runtime::{SessionStore, ShellTarget};
use winx_code_agent::state::pty::{PtyShell, SharedPtyShell};

#[test]
fn session_store_resolves_main_shell_by_thread() {
    let mut store = SessionStore::new();
    let shell: SharedPtyShell = Arc::new(Mutex::new(None::<PtyShell>));

    store.bind_main("thread-a", &shell);

    let resolved = store.resolve("thread-a", &ShellTarget::Main).expect("main shell is indexed");
    assert!(Arc::ptr_eq(&shell, &resolved));
    assert!(store.resolve("thread-b", &ShellTarget::Main).is_none());
}
