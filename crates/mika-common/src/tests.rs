//! Tests for config helpers.

#[test]
fn home_dir_honours_mika_home_env() {
    unsafe { std::env::set_var("MIKA_HOME", "C:\\mika\\.mikatmp") };
    let home = super::home_dir().unwrap();
    assert!(home.ends_with(".mikatmp"), "{home:?}");
    unsafe { std::env::remove_var("MIKA_HOME") };
}

#[test]
fn mesh_map_path_lives_under_home() {
    unsafe { std::env::set_var("MIKA_HOME", "C:\\mika\\.mikatmp2") };
    let path = super::mesh_map_path().unwrap();
    assert!(path.ends_with("mesh"), "{path:?}");
    assert!(path.parent().unwrap().ends_with(".mikatmp2"));
    unsafe { std::env::remove_var("MIKA_HOME") };
}
