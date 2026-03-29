macro_rules! debug_log_path {
    () => { concat!(env!("CARGO_MANIFEST_DIR"), "/debug_log_6.txt") };
}

macro_rules! debug_eprintln {
    ($($arg:tt)*) => {{
        #[cfg(any(test, feature = "debug"))]
        {
            eprintln!($($arg)*);
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(debug_log_path!())
            {
                let _ = writeln!(f, $($arg)*);
            }
        }
    }};
}

macro_rules! debug_msg {
    ($($arg:tt)*) => {{
        #[cfg(any(test, feature = "benchmark"))]
        {
            anchor_lang::prelude::msg!($($arg)*);
        }
    }};
}

#[cfg(test)]
pub fn clear_debug_log() {
    let _ = std::fs::write(debug_log_path!(), "");
}
