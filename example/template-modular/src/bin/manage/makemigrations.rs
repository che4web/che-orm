use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use che_orm::{Migration, MigrationLayout, ProjectState, render_migration_rust};

use template_modular::apps;

use template_modular::{migrations, registry};

use super::Result;

pub fn run(args: &[String]) -> Result<()> {
    let mut name = None;
    let mut empty = false;
    let mut merge = false;
    let mut owner = None;
    let mut policy = che_orm::GenerationPolicy::default();
    let mut check = false;
    let mut dry_run = false;
    let mut options = args.iter();
    while let Some(option) = options.next() {
        match option.as_str() {
            "--name" => name = Some(options.next().ok_or("--name requires a value")?.as_str()),
            "--empty" => empty = true,
            "--merge" => merge = true,
            "--rename-model" | "--rename-field" => {
                let from = options.next().ok_or("rename requires OLD NEW")?;
                let to = options.next().ok_or("rename requires OLD NEW")?;
                let (scope, old) = from
                    .split_once('.')
                    .ok_or("use APP.OldModel or TABLE.old_column")?;
                let mappings = if option == "--rename-model" {
                    &mut policy.rename_models
                } else {
                    &mut policy.rename_columns
                };
                if mappings
                    .insert((scope.into(), old.into()), to.clone())
                    .is_some()
                {
                    return Err(format!("duplicate rename for {from}").into());
                }
            }
            app if !app.starts_with('-') && owner.is_none() => owner = Some(app),
            "--check" => check = true,
            "--drop" => {
                policy.allow_drop.insert(
                    options
                        .next()
                        .ok_or("--drop requires table or table.column")?
                        .clone(),
                );
            }
            "--dry-run" => dry_run = true,
            _ => return Err(format!("unknown makemigrations option: {option}").into()),
        }
    }
    if check && dry_run {
        return Err("use either --check or --dry-run".into());
    }
    if empty && merge
        || (empty || merge)
            && (check
                || !policy.allow_drop.is_empty()
                || !policy.rename_models.is_empty()
                || !policy.rename_columns.is_empty())
    {
        return Err("--empty and --merge each take an optional app, --name and --dry-run".into());
    }
    if owner.is_some() && !(empty || merge) {
        return Err("an app argument requires --empty or --merge".into());
    }
    let name = name.unwrap_or(if merge {
        "merge"
    } else if empty {
        "manual"
    } else {
        "auto"
    });
    if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
        return Err(
            "migration name must contain only ASCII letters, digits and underscores".into(),
        );
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    // Serialize source updates and reject a management binary built from older history.

    let lock_path = root.join("src/apps/.createapp.lock");

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)?;
    let _lock = SourceLock(lock_path);

    if fs::read_to_string(root.join("src/apps/mod.rs"))? != apps::REGISTRY_SOURCE {
        return Err(
            "app registry changed; rebuild manage with cargo run before generating again".into(),
        );
    }
    let sources = apps::migration_sources();
    for source in &sources {
        ensure_current_sources(
            &migration_dir(root, source.app),
            source.registry,
            &source.files,
        )?;
    }

    let graph = migrations::graph()?;
    let registry = registry();
    let layout = MigrationLayout::PerApp;
    let generated = if empty || merge {
        let mut owners = match layout {
            MigrationLayout::Single(app) => vec![app],
            MigrationLayout::PerApp => registry.apps().to_vec(),
        };
        if matches!(layout, MigrationLayout::PerApp)
            && (owner == Some("che_sync") || !graph.app_heads("che_sync").is_empty())
        {
            owners.push("che_sync");
        }
        if let Some(app) = owner {
            if !owners.contains(&app) {
                return Err(format!("unknown migration owner {app}").into());
            }
            owners = vec![app];
        } else if merge {
            owners.retain(|app| graph.app_heads(app).len() > 1);
        }
        if owners.is_empty() {
            return Err("no conflicting migration heads".into());
        }
        if empty && owners.len() != 1 {
            return Err("specify the migration owner after --empty".into());
        }
        owners
            .into_iter()
            .map(|app| graph.manual_migration(app, name, merge))
            .collect::<std::result::Result<Vec<_>, _>>()?
    } else {
        let desired = ProjectState::from_app_schemas(registry.app_schemas())?;
        graph.generate_with_policy(&desired, layout, name, &policy)?
    };
    if generated.is_empty() {
        println!("No model changes detected.");
        return Ok(());
    }
    for migration in &generated {
        println!("{}:{}", migration.id.app, migration.id.name);
        for operation in &migration.operations {
            println!("  {}", operation.summary());
        }
    }
    if check {
        return Err(
            "model changes require a migration; run template-modular-manage makemigrations".into(),
        );
    }

    for migration in &generated {
        if !sources.iter().any(|source| source.app == migration.id.app) {
            return Err(format!("no migration crate registered for {}", migration.id.app).into());
        }
    }
    let proposed: Vec<_> = migrations::all()
        .into_iter()
        .chain(generated.iter().cloned())
        .collect();
    if dry_run {
        for migration in &generated {
            println!("// {}:{}", migration.id.app, migration.id.name);
            print!("{}", render_migration_rust(migration));
        }
        return Ok(());
    }
    // Format every file before touching the source tree.
    let mut writes = Vec::new();
    for migration in &generated {
        let dir = migration_dir(root, &migration.id.app);
        writes.push((
            dir.join(format!("m{}.rs", migration.id.name)),
            format_rust(render_migration_rust(migration))?,
            None,
        ));
    }
    for source in sources {
        if generated
            .iter()
            .any(|migration| migration.id.app == source.app)
        {
            let dir = migration_dir(root, source.app);
            let history = proposed
                .iter()
                .filter(|migration| migration.id.app == source.app);
            writes.push((
                dir.join("mod.rs"),
                format_rust(render_registry(history))?,
                Some(source.registry),
            ));
        }
    }
    write_sources(&writes)?;
    for migration in &generated {
        println!(
            "Created {}:{} in {}",
            migration.id.app,
            migration.id.name,
            migration_dir(root, &migration.id.app).display()
        );
    }

    println!(
        "Review the operations, then run cargo run -p template-modular --bin template-modular-manage -- migrate."
    );
    Ok(())
}

fn migration_dir(root: &Path, app: &str) -> PathBuf {
    if app == "che_sync" {
        return root.join("src/che_sync_migrations");
    }
    root.join("apps").join(app).join("src/migrations")
}

fn write_sources(writes: &[(PathBuf, String, Option<&str>)]) -> Result<()> {
    let mut written = Vec::new();
    let result = (|| -> Result<()> {
        for (path, source, original) in writes {
            if original.is_some() {
                super::createapp::replace_source(path, source)?;
                written.push((path, original));
            } else {
                let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
                written.push((path, original));
                file.write_all(source.as_bytes())?;
            }
        }
        Ok(())
    })();
    if result.is_err() {
        for (path, original) in written.into_iter().rev() {
            let restored = match original {
                Some(source) => super::createapp::replace_source(path, source),
                None => fs::remove_file(path).map_err(Into::into),
            };
            if let Err(error) = restored {
                eprintln!("could not restore {}: {error}", path.display());
            }
        }
    }
    result
}

fn ensure_current_sources(dir: &Path, registry_source: &str, files: &[(&str, &str)]) -> Result<()> {
    let expected: BTreeSet<_> = files.iter().map(|(name, _)| (*name).to_owned()).collect();
    let actual = fs::read_dir(dir)?
        .map(|entry| {
            let entry = entry?;
            Ok(entry.file_name().to_string_lossy().into_owned())
        })
        .collect::<std::io::Result<BTreeSet<_>>>()?
        .into_iter()
        .filter(|name| name != "mod.rs" && name.ends_with(".rs"))
        .collect::<BTreeSet<_>>();
    let mut matches =
        actual == expected && fs::read_to_string(dir.join("mod.rs"))? == registry_source;
    for (name, source) in files {
        matches &= fs::read_to_string(dir.join(name))? == *source;
    }
    if !matches {
        return Err(
            "migration sources changed; rebuild manage with cargo run before generating again"
                .into(),
        );
    }
    Ok(())
}

pub(super) fn render_registry<'a>(history: impl IntoIterator<Item = &'a Migration>) -> String {
    let modules: Vec<_> = history
        .into_iter()
        .map(|migration| format!("m{}", migration.id.name))
        .collect();
    let mut source = String::new();
    for module in &modules {
        source.push_str(&format!("mod {module};\n"));
    }

    source.push_str("\nuse che_orm::Migration;\n\n");

    source.push_str("pub const REGISTRY_SOURCE: &str = include_str!(\"mod.rs\");\n\n");
    source.push_str("pub fn sources() -> Vec<(&'static str, &'static str)> {\n    vec![\n");
    for module in &modules {
        source.push_str(&format!(
            "(\"{module}.rs\", include_str!(\"{module}.rs\")),\n"
        ));
    }
    source.push_str("]\n}\n\npub fn all() -> Vec<Migration> {\n    vec![\n");
    for module in &modules {
        source.push_str(&format!("{module}::migration(),\n"));
    }
    source.push_str("]\n}\n");

    source
}

pub(super) fn format_rust(source: String) -> Result<String> {
    let mut child = Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "stdout"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("rustfmt stdin is unavailable")?;
    let writer = std::thread::spawn(move || stdin.write_all(source.as_bytes()));
    let output = child.wait_with_output()?;
    writer.join().map_err(|_| "rustfmt input thread failed")??;
    if !output.status.success() {
        return Err(format!(
            "rustfmt failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

struct SourceLock(PathBuf);

impl Drop for SourceLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
