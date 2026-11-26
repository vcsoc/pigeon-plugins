//! Resource writer unit checks (the ring behaviour is covered in runtime_tests).

use super::*;

#[test]
fn ring_has_at_least_three_slots() {
    assert!(RING_SLOTS >= 3);
}

#[test]
fn sanitize_keeps_safe_characters_only() {
    assert_eq!(sanitize("thumb:42/a b"), "thumb_42_a_b");
    assert_eq!(sanitize("ok-name_1.2"), "ok-name_1_2");
}
