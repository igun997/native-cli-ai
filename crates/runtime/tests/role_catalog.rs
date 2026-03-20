use nca_runtime::role_catalog::RoleCatalog;
use std::path::Path;

#[test]
fn test_role_catalog_loads_built_in_roles() {
    // Use a path that won't have any .nca/roles/ directory,
    // so only built-in roles are loaded.
    let catalog = RoleCatalog::load(Path::new("."));
    assert!(catalog.get("researcher").is_some());
    assert!(catalog.get("implementer").is_some());
    assert!(catalog.get("reviewer").is_some());
    assert!(catalog.get("tester").is_some());
    assert!(catalog.get("architect").is_some());
    assert!(catalog.get("debugger").is_some());
    assert_eq!(catalog.list().len(), 6);
}
