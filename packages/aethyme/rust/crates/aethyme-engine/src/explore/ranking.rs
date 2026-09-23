pub(super) fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.contains(".test.")
        || lower.contains(".spec.")
        || lower.contains("_test.")
}
