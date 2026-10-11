//! Visual smoke test: shows the "clip saved" toast for a few seconds.
fn main() {
    clipcheckd::toast::clip_saved(120);
    std::thread::sleep(std::time::Duration::from_secs(4));
}
