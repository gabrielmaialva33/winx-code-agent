#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    winx_code_agent::fuzzing::normalize_thread_id(data);
});
