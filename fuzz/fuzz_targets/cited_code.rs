#![no_main]

use libfuzzer_sys::fuzz_target;
use std::path::Path;
use winx_code_agent::tools::cited_code;

fuzz_target!(|text: &str| {
    if let Some(specs) = cited_code::read_specs(text, Path::new(".")) {
        assert!(specs.len() <= cited_code::MAX_LOCATIONS);
    }
});
