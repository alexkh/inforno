use inforno_core::realm::{
    ActiveRealm, Actor, Cap, GlobExpr, Power, RealmConfig, RealmMountConfig,
    RoleConfig, Tier,
};
use inforno_core::realm_mount::VfsMaskSession;
use inforno_core::realm_spawn::spawn_masked_command;
use indexmap::IndexMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Define a minimal dummy realm config mounting the current directory
    let mut mounts = IndexMap::new();
    mounts.insert(
        "/workspace".to_string(),
        RealmMountConfig {
            host: std::env::current_dir()?,
            read_only: false,
            hide_if: None,
            read_only_if: None,
            wildcards: vec![],
            ignore: vec![],
            description: None,
            kind: None,
            create_rules: vec![],
        },
    );

    let mut roles = IndexMap::new();
    roles.insert(
        "tester".to_string(),
        RoleConfig {
            tier: Tier(2),
            description: "Test Role".to_string(),
            // Overrides are attached directly to the role config so they don't broadly cascade
            powers: vec![Power {
                span: GlobExpr::Match(vec![
                    "**/.gitignore".to_string(),
                    "**/.env".to_string(),
                    "**/.cargo".to_string(),
                    "**/.cargo/**".to_string(),
                ]),
                caps: vec![Cap::Read, Cap::Write, Cap::Create],
                memo: Some("Unhide specific dotfiles".to_string()),
                overrides: Some("dotfiles".to_string()),
            }],
        },
    );

    let mut tiers = std::collections::BTreeMap::new();
    tiers.insert(
        2,
        vec![Power {
            span: GlobExpr::Match(vec!["**".to_string()]),
            caps: vec![Cap::Read, Cap::Write, Cap::Create],
            memo: Some("Full access".to_string()),
            overrides: None,
        }],
    );

    let config = RealmConfig {
        default_workspace: None,
        hide_if: None,
        read_only_if: None,
        expressions: IndexMap::new(),
        wildcards: IndexMap::new(),
        mounts,
        create_rules: vec![],
        roles,
        tiers,
    };

    // 2. Compile realm and spawn the FUSE background session
    let active_realm = ActiveRealm::from_config("test_realm".to_string(), config)?;
    let actor = Actor {
        roles: vec!["tester".to_string()],
    };
    let vfs_session = VfsMaskSession::spawn_vfs(active_realm, actor)?;

    // Give FUSE a moment to fully initialize in the background thread
    // before we try to bind-mount host folders into its synthetic directories.
    //std::thread::sleep(std::time::Duration::from_millis(200));

    println!("FUSE Mount ready at: {:?}", vfs_session.mount_path());
    println!("Dropping into restricted bash shell. Type 'exit' to leave.\n");

    // 3. Spawn the masked bash shell (empty cmd string triggers interactive bash)
    // std::process::Command inherits stdin/stdout/stderr by default.
    let mut child = spawn_masked_command(vfs_session.mount_path(), "")?;

    // 4. Wait for the user to exit the shell before dropping/cleaning the FUSE mount
    let status = child.wait()?;
    println!("\nShell exited with status: {}", status);

    Ok(())
}
