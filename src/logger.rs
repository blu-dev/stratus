use std::collections::HashMap;

use log::Level;

unsafe extern "C" {
    #[link_name = "_ZN2nn4diag6detail16PrintDebugStringEPKcm"]
    unsafe fn print_debug_string(ptr: *const i8, len: usize);
}

extern "C" {
    fn skyline_tcp_send_raw(bytes: *const u8, usize: u64);
}

#[skyline::hook(replace = skyline_tcp_send_raw)]
unsafe fn send_raw_hook(bytes: *const u8, size: usize) {
    print_debug_string(bytes.cast(), size);
}

pub fn install_hooks() {
    skyline::install_hooks!(send_raw_hook);
}

pub struct NxKernelLogger {
    by_module: HashMap<&'static str, Level>,
}

impl NxKernelLogger {
    pub fn new() -> Self {
        Self {
            by_module: HashMap::new(),
        }
    }

    pub fn module(mut self, module: &'static str, level: Level) -> Self {
        self.by_module.insert(module, level);
        self
    }
}

impl log::Log for NxKernelLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= Level::Trace
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            if record
                .module_path_static()
                .and_then(|module| {
                    self.by_module
                        .get(module.trim_start_matches(concat!(env!("CARGO_CRATE_NAME"), "::")))
                })
                .map(|filter| record.level() <= *filter)
                .unwrap_or_else(|| record.level() <= Level::Info)
            {
                let message = format!("[{: <5}]  {}", record.level(), record.args());
                unsafe { print_debug_string(message.as_ptr().cast(), message.len()) };
            }
        }
    }

    fn flush(&self) {}
}
