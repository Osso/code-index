use std::sync::Mutex;

pub(crate) static CWD_LOCK: Mutex<()> = Mutex::new(());
