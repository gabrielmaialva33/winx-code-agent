use std::sync::Arc;

use tokio::sync::Mutex;

use winx_code_agent::runtime::{SessionStore, ShellTarget};
use winx_code_agent::state::pty::{PtyShell, SharedPtyShell};

#[test]
fn session_store_resolves_main_shell_by_thread() {
    let mut store = SessionStore::new();
    let shell: SharedPtyShell = Arc::new(Mutex::new(None::<PtyShell>));

    store.bind_main("thread-a", &shell);

    assert!(
        store
            .resolve("thread-a", &ShellTarget::Main)
            .is_some_and(|resolved| Arc::ptr_eq(&shell, &resolved)),
        "main shell is indexed"
    );
    assert!(store.resolve("thread-b", &ShellTarget::Main).is_none());
}
