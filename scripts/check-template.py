#!/usr/bin/env python3
"""Generate and verify standalone applications against local or published che-orm."""

import argparse
import json
from itertools import product
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile


def run(command, *, cwd, env):
    print("$ " + " ".join(command), flush=True)
    subprocess.run(command, cwd=cwd, env=env, check=True)


def expect_failure(command, *, cwd, env, message):
    print("$ " + " ".join(command), flush=True)
    result = subprocess.run(command, cwd=cwd, env=env, text=True, capture_output=True)
    print(result.stdout + result.stderr, flush=True)
    assert result.returncode != 0, "command unexpectedly succeeded"
    assert message in result.stdout + result.stderr


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--backend", choices=["sqlite", "postgres"])
    parser.add_argument("--layout", choices=["single", "modular"])
    parser.add_argument(
        "--published", action="store_true",
        help="use crates.io instead of patching core and macros to this checkout",
    )
    args = parser.parse_args()
    # Optional live check: this URL must point to a disposable, empty database.
    postgres_url = os.environ.get("TEMPLATE_TEST_POSTGRES_URL")
    root = Path(__file__).resolve().parents[1]
    env = os.environ.copy()
    # Keep prebuilt management binaries separate when local/published checks run together.
    target = Path(env.get("CARGO_TARGET_DIR", root / "target" / "template-check")).resolve()
    env["CARGO_TARGET_DIR"] = str(target / ("published" if args.published else "local"))
    # Schema export must work without opening this intentionally invalid URL.
    env["CHE_ORM_DATABASE_URL"] = "invalid://template-schema-check"
    patches = [] if args.published else [
        "--config", f'patch.crates-io.che-orm.path="{root.as_posix()}"',
        "--config", f'patch.crates-io.che-orm-macros.path="{(root / "che-orm-macros").as_posix()}"',
    ]

    def cargo(*args):
        return ["cargo", args[0], *patches, *args[1:]]

    backends = [args.backend] if args.backend else ["sqlite", "postgres"]
    layouts = [args.layout] if args.layout else ["single", "modular"]
    for backend, layout in product(backends, layouts):
        with tempfile.TemporaryDirectory(prefix="che-orm-template-") as directory:
            name = f"orm-template-{backend}-{layout}"
            # Also exercise generation inside an existing Cargo workspace.
            parent_manifest = Path(directory) / "Cargo.toml"
            parent_contents = '[workspace]\nmembers = []\n'
            parent_manifest.write_text(parent_contents)
            run(
                [
                    "cargo", "generate", "--path", str(root / "templates" / "che-orm"),
                    "--name", name, "--destination", directory,
                    "--define", f"backend={backend}", "--define", f"layout={layout}",
                    "--silent", "--vcs", "none",
                    "--no-workspace",
                ],
                cwd=root,
                env=env,
            )
            assert parent_manifest.read_text() == parent_contents
            project = Path(directory) / name
            modular = layout == "modular"
            tasks_src = project / ("apps/tasks/src" if modular else "src")
            model = tasks_src / "models.rs"
            assert model.exists()
            assert '#[orm(app = super::APP_NAME, index("title"))]' in model.read_text()
            assert (project / "src/models.rs").exists() == (layout == "single")
            assert (project / "apps/tasks/Cargo.toml").exists() == modular
            assert not (project / "src/apps/tasks").exists()
            assert (project / "src/migrations.rs").exists() == modular
            assert not modular or not (project / "src/migrations").exists()
            migration_dir = tasks_src / "migrations"
            if modular:
                run(cargo("test", "-p", f"{name}-app-tasks", "--features", backend),
                    cwd=project, env=env)
            empty = {p.name: p.read_bytes() for p in migration_dir.glob("*.rs")}
            assert set(empty) == {"mod.rs"}
            manage = lambda *args: cargo("run", "--bin", "manage", "--", *args)
            expect_failure(manage("makemigrations", "--check"), cwd=project, env=env,
                           message="model changes require a migration")
            run(manage("makemigrations", "--dry-run"), cwd=project, env=env)
            assert empty == {p.name: p.read_bytes() for p in migration_dir.glob("*.rs")}
            run(manage("makemigrations", "--name", "initial"), cwd=project, env=env)
            generated_initial = migration_dir / "m0001_initial.rs"
            assert generated_initial.exists()
            assert "CreateModel" in generated_initial.read_text()
            assert 'name: "tasks_task"' in generated_initial.read_text()
            for command in [
                ["cargo", "fmt", "--check"],
                cargo("test"),
                cargo("clippy", "--all-targets", "--", "-D", "warnings"),
                cargo("run", "--bin", "manage", "--", "schema"),
            ]:
                run(command, cwd=project, env=env)

            if backend == "sqlite":
                runtime_env = dict(env, CHE_ORM_DATABASE_URL=str(project / "app.db"))
                for command in [
                    cargo("run", "--bin", "manage", "--", "migrate"),
                    cargo("run", "--bin", "manage", "--", "migrate"),
                    cargo("run", "--bin", "manage", "--", "migrate", "status"),
                    cargo("run"),
                ]:
                    run(command, cwd=project, env=runtime_env)
            else:
                run(
                    cargo("test", "--no-default-features", "--features", "sqlite"),
                    cwd=project,
                    env=env,
                )

            # Generate a real follow-up migration without opening the configured URL.
            initial = {p.name: p.read_bytes() for p in migration_dir.glob("*.rs")}
            run(manage("makemigrations", "--check"), cwd=project, env=env)
            run(manage("makemigrations"), cwd=project, env=env)
            assert initial == {p.name: p.read_bytes() for p in migration_dir.glob("*.rs")}

            runtime_env = dict(env, CHE_ORM_DATABASE_URL=str(project / "app.db"))
            migrate = cargo(
                "run", "--no-default-features", "--features", "sqlite",
                "--bin", "manage", "--", "migrate",
            )
            run(migrate, cwd=project, env=runtime_env)
            with sqlite3.connect(project / "app.db") as db:
                db.execute("INSERT INTO tasks_task (title) VALUES (?)", ("Preserve existing row",))

            model.write_text(model.read_text().replace(
                "    pub title: String,", "    pub title: String,\n    pub description: Option<String>,",
            ))
            expect_failure(manage("makemigrations", "--check"), cwd=project, env=env,
                           message="model changes require a migration")
            run(manage("makemigrations", "--dry-run"), cwd=project, env=env)
            expect_failure(manage("makemigrations", "--name", "../bad"), cwd=project,
                           env=env, message="migration name must contain only ASCII")
            assert initial == {p.name: p.read_bytes() for p in migration_dir.glob("*.rs")}

            metadata = json.loads(subprocess.check_output(
                cargo("metadata", "--no-deps", "--format-version", "1"),
                cwd=project, env=env, text=True,
            ))
            stale = project / "stale-manage"
            shutil.copy2(Path(metadata["target_directory"]) / "debug" / "manage", stale)
            run(manage("makemigrations", "--name", "add_description"), cwd=project, env=env)
            generated = migration_dir / "m0002_add_description.rs"
            assert generated.exists()
            assert "AddColumn" in generated.read_text()
            assert "0001_initial" in generated.read_text()
            assert (migration_dir / "m0001_initial.rs").read_bytes() == initial["m0001_initial.rs"]
            expect_failure([str(stale), "makemigrations"], cwd=project, env=env,
                           message="migration sources changed")
            run(manage("makemigrations", "--check"), cwd=project, env=env)
            run(manage("makemigrations"), cwd=project, env=env)
            assert len(list(migration_dir.glob("m[0-9]*.rs"))) == 2
            for command in [
                ["cargo", "fmt", "--check"], cargo("test"),
                cargo("clippy", "--all-targets", "--", "-D", "warnings"), migrate,
            ]:
                run(command, cwd=project, env=runtime_env)
            with sqlite3.connect(project / "app.db") as db:
                assert db.execute(
                    "SELECT description FROM tasks_task WHERE title = ?", ("Preserve existing row",),
                ).fetchone() == (None,)

            # New modules must compile, register their own schema owner and migrate with FKs.
            run(manage("makemigrations", "--check"), cwd=project, env=env)
            shutil.copy2(Path(metadata["target_directory"]) / "debug" / "manage", stale)
            for invalid in ["../escape", "mod", "register", "che_orm", "bad-name"]:
                expect_failure(manage("createapp", invalid), cwd=project, env=env,
                               message="app name must be")
            expect_failure(manage("createapp", "tasks"), cwd=project, env=env,
                           message="already registered")
            if modular:
                manifest = project / "Cargo.toml"
                original_manifest = manifest.read_text()
                registry_file = project / "src/apps/mod.rs"
                original_registry = registry_file.read_bytes()
                for alias in ["app_blog", "app-blog"]:
                    occupied = original_manifest + f'\n[dependencies.{alias}]\npackage = "serde"\nversion = "1"\n'
                    manifest.write_text(occupied)
                    expect_failure(manage("createapp", "blog"), cwd=project, env=env,
                                   message="dependency alias app_blog is already in use")
                    assert manifest.read_text() == occupied
                    assert registry_file.read_bytes() == original_registry
                    assert not (project / "apps/blog").exists()
                manifest.write_text(original_manifest)
            run(manage("createapp", "blog"), cwd=project, env=env)
            apps = project / ("apps" if modular else "src/apps")
            blog = apps / "blog"
            blog_src = blog / "src" if modular else blog
            app_mod = blog_src / ("lib.rs" if modular else "mod.rs")
            assert app_mod.exists() and (blog_src / "models.rs").exists()
            assert 'pub const APP_NAME: &str = "blog";' in app_mod.read_text()
            assert '#[orm(app = super::APP_NAME)]' in (blog_src / "models.rs").read_text()
            expect_failure([str(stale), "createapp", "other"], cwd=project, env=env,
                           message="app registry changed")
            assert not (apps / "other").exists()
            run(manage("makemigrations", "--check"), cwd=project, env=env)
            before = {p.relative_to(blog): p.read_bytes() for p in blog.rglob("*") if p.is_file()}
            expect_failure(manage("createapp", "blog"), cwd=project, env=env,
                           message="already registered")
            assert before == {p.relative_to(blog): p.read_bytes() for p in blog.rglob("*") if p.is_file()}
            if modular:
                with (blog / "Cargo.toml").open("a") as manifest:
                    manifest.write(f'app_tasks = {{ package = "{name}-app-tasks", path = "../tasks" }}\n')
            task_import = "app_tasks::models::Task" if modular else "crate::models::Task"
            (blog_src / "models.rs").write_text(
                f'use che_orm::Model;\nuse {task_import};\n\n'
                '#[derive(Debug, Model)]\n#[orm(app = super::APP_NAME)]\n'
                'pub struct Post {\n    #[orm(primary_key)]\n    pub id: i64,\n'
                '    #[orm(foreign_key = Task, on_delete = "cascade")]\n    pub task_id: i64,\n}\n'
            )
            app_mod.write_text(app_mod.read_text().replace(
                "SchemaSet::new()", "SchemaSet::new().model::<models::Post>()",
            ))
            run(["cargo", "fmt"], cwd=project, env=env)
            if modular:
                fresh = Path(directory) / "fresh-history"
                shutil.copytree(project, fresh, ignore=shutil.ignore_patterns("stale-manage", "app.db*"))
                fresh_tasks = fresh / "apps/tasks/src/migrations"
                for path in fresh_tasks.glob("*.rs"):
                    path.unlink()
                for filename, contents in empty.items():
                    (fresh_tasks / filename).write_bytes(contents)
                # Both copies have a binary named `manage` but embed different sources.
                fresh_env = dict(env, CARGO_TARGET_DIR=str(Path(env["CARGO_TARGET_DIR"]) / "fresh-history"))
                run(manage("makemigrations", "--name", "initial"), cwd=fresh, env=fresh_env)
                first_tasks = (fresh_tasks / "m0001_initial.rs").read_text()
                first_blog = (fresh / "apps/blog/src/migrations/m0001_initial.rs").read_text()
                assert 'app: "blog"' not in first_tasks
                assert 'app: "tasks"' in first_blog
                fresh_url = str(fresh / "app.db") if backend == "sqlite" else postgres_url
                if fresh_url:
                    db_env = dict(fresh_env, CHE_ORM_DATABASE_URL=fresh_url)
                    run(manage("migrate"), cwd=fresh, env=db_env)

                # A follow-up adds a new FK and retargets an existing FK to a new
                # table. The new tables also have reciprocal FKs in one crate.
                fresh_models = fresh / "apps/tasks/src/models.rs"
                with fresh_models.open("a") as source:
                    source.write('''
#[derive(Debug, Model)]
#[orm(app = super::APP_NAME)]
pub struct Label {
    #[orm(primary_key)]
    pub id: i64,
    #[orm(foreign_key = Category)]
    pub category_id: Option<i64>,
}

#[derive(Debug, Model)]
#[orm(app = super::APP_NAME)]
pub struct Category {
    #[orm(primary_key)]
    pub id: i64,
    #[orm(foreign_key = Label)]
    pub label_id: Option<i64>,
}
''')
                tasks_lib = fresh / "apps/tasks/src/lib.rs"
                tasks_lib.write_text(tasks_lib.read_text().replace(
                    ".model::<models::Task>()", ".model::<models::Task>().model::<models::Label>().model::<models::Category>()",
                ))
                fresh_blog = fresh / "apps/blog/src/models.rs"
                fresh_blog.write_text(fresh_blog.read_text().replace(
                    "use app_tasks::models::Task;", "use app_tasks::models::Label;",
                ).replace("foreign_key = Task", "foreign_key = Label").replace(
                    "    pub task_id: i64,",
                    '    pub task_id: i64,\n    #[orm(foreign_key = Label)]\n    pub label_id: Option<i64>,',
                ))
                run(manage("makemigrations", "--name", "labels"), cwd=fresh, env=fresh_env)
                labels = (fresh_tasks / "m0002_labels.rs").read_text()
                assert 'name: "tasks_label"' in labels and 'name: "tasks_category"' in labels
                blog_labels = (fresh / "apps/blog/src/migrations/m0002_labels.rs").read_text()
                assert 'app: "tasks"' in blog_labels and 'name: "0002_labels"' in blog_labels
                assert "AddColumn" in blog_labels and "AlterColumn" in blog_labels
                run(manage("makemigrations", "--check"), cwd=fresh, env=fresh_env)
                if fresh_url:
                    run(manage("migrate"), cwd=fresh, env=db_env)
                    run(manage("migrate", "status"), cwd=fresh, env=db_env)
                    def query(sql):
                        if backend == "postgres":
                            return subprocess.check_output(
                                ["psql", fresh_url, "-XAtq", "-v", "ON_ERROR_STOP=1", "-c", sql], text=True,
                            ).strip()
                        with sqlite3.connect(fresh_url) as connection:
                            connection.execute("PRAGMA foreign_keys = ON")
                            row = connection.execute(sql).fetchone()
                            return str(row[0]) if row else ""

                    query("INSERT INTO tasks_label (id) VALUES (41)")
                    query("INSERT INTO blog_post (id, task_id, label_id) VALUES (7, 41, 41)")
                # Rename the FK target while adding another FK in the child crate.
                fresh_models.write_text(fresh_models.read_text().replace(
                    '#[orm(app = super::APP_NAME)]\npub struct Label',
                    '#[orm(app = super::APP_NAME, table = "renamed_labels")]\npub struct Label',
                ))
                fresh_blog.write_text(fresh_blog.read_text().replace(
                    '    pub label_id: Option<i64>,',
                    '    pub label_id: Option<i64>,\n    #[orm(foreign_key = Label)]\n    pub other_label_id: Option<i64>,',
                ))
                run(manage("makemigrations", "--name", "rename"), cwd=fresh, env=fresh_env)
                if fresh_url:
                    run(manage("migrate"), cwd=fresh, env=db_env)
                    assert query("SELECT label_id FROM blog_post WHERE id = 7") == "41"
                    assert query("SELECT id FROM renamed_labels WHERE id = 41") == "41"

                # Remove every inbound FK, but keep the child table and its row.
                fresh_blog.write_text('use che_orm::Model;\n#[derive(Debug, Model)]\n'
                                      '#[orm(app = super::APP_NAME)]\npub struct Post {\n'
                                      '    #[orm(primary_key)]\n    pub id: i64,\n}\n')
                fresh_models.write_text(fresh_models.read_text().replace(
                    '    #[orm(foreign_key = Label)]\n    pub label_id: Option<i64>,', '',
                ))
                tasks_lib.write_text(tasks_lib.read_text().replace('.model::<models::Label>()', ''))
                drop_fields = ["renamed_labels", "blog_post.task_id", "blog_post.label_id", "blog_post.other_label_id", "tasks_category.label_id"]
                run(manage("makemigrations", "--name", "retire_labels", *(arg for field in drop_fields for arg in ("--drop", field))), cwd=fresh, env=fresh_env)
                if fresh_url:
                    run(manage("migrate"), cwd=fresh, env=db_env)
                    assert query("SELECT id FROM blog_post WHERE id = 7") == "7"

                # Retire a related pair while preserving the unrelated Post row.
                fresh_blog.write_text(fresh_blog.read_text() + '\nuse app_tasks::models::Category;\n'
                    '#[derive(Debug, Model)]\n#[orm(app = super::APP_NAME)]\npub struct Link {\n'
                    '    #[orm(primary_key)]\n    pub id: i64,\n'
                    '    #[orm(foreign_key = Category)]\n    pub category_id: i64,\n}\n')
                fresh_blog_lib = fresh / "apps/blog/src/lib.rs"
                original = fresh_blog_lib.read_text()
                fresh_blog_lib.write_text(original.replace('.model::<models::Post>()',
                                                          '.model::<models::Post>().model::<models::Link>()'))
                run(manage("makemigrations", "--name", "add_link"), cwd=fresh, env=fresh_env)
                if fresh_url:
                    run(manage("migrate"), cwd=fresh, env=db_env)
                    query("INSERT INTO tasks_category (id) VALUES (9)")
                    query("INSERT INTO blog_link (id, category_id) VALUES (1, 9)")
                fresh_blog_lib.write_text(original)
                tasks_lib.write_text(tasks_lib.read_text().replace('.model::<models::Category>()', ''))
                expect_failure(manage("makemigrations", "--name", "drop_pair"), cwd=fresh, env=fresh_env, message="manual migration required")
                run(manage("makemigrations", "--name", "drop_pair", "--drop", "tasks_category", "--drop", "blog_link"), cwd=fresh, env=fresh_env)
                run(manage("makemigrations", "--check"), cwd=fresh, env=fresh_env)
                if fresh_url:
                    run(manage("migrate"), cwd=fresh, env=db_env)
                    assert query("SELECT id FROM blog_post WHERE id = 7") == "7"
                    result = subprocess.check_output(manage("migrate"), cwd=fresh, env=db_env, text=True)
                    assert "Applied 0 migration(s)" in result
            history = {p.name: p.read_bytes() for p in migration_dir.glob("*.rs") if p.name != "mod.rs"}
            run(manage("makemigrations", "--name", "add_blog"), cwd=project, env=env)
            blog_migration = (blog_src / "migrations/m0001_add_blog.rs" if modular
                              else migration_dir / "m0003_add_blog.rs")
            assert blog_migration.exists() and 'app: "blog"' in blog_migration.read_text()
            assert 'name: "blog_post"' in blog_migration.read_text()
            if modular:
                # The Post FK only requires the initial primary key on Task.
                assert 'name: "0001_initial"' in blog_migration.read_text()
                assert 'name: "0002_add_description"' not in blog_migration.read_text()
                assert not (migration_dir / "m0003_add_blog.rs").exists()
            for filename, contents in history.items():
                assert (migration_dir / filename).read_bytes() == contents
            for command in [
                manage("makemigrations", "--check"), ["cargo", "fmt", "--check"],
                cargo("test"), cargo("clippy", "--all-targets", "--", "-D", "warnings"), migrate,
            ]:
                run(command, cwd=project, env=runtime_env)
            with sqlite3.connect(project / "app.db") as db:
                db.execute("PRAGMA foreign_keys = ON")
                task_id = db.execute(
                    "SELECT id FROM tasks_task WHERE title = ?", ("Preserve existing row",),
                ).fetchone()[0]
                db.execute("INSERT INTO blog_post (task_id) VALUES (?)", (task_id,))
                assert db.execute("SELECT task_id FROM blog_post").fetchone() == (task_id,)

            if modular:
                model.write_text(model.read_text().replace(
                    "    pub title: String,", "    pub title: String,\n    pub notes: Option<String>,",
                ))
                blog_model = blog_src / "models.rs"
                blog_model.write_text(blog_model.read_text().replace(
                    "    pub task_id: i64,", "    pub task_id: i64,\n    pub body: Option<String>,",
                ))
                run(manage("makemigrations", "--name", "update_both"), cwd=project, env=env)
                assert (migration_dir / "m0003_update_both.rs").exists()
                assert (blog_src / "migrations/m0002_update_both.rs").exists()
                run(manage("makemigrations", "--check"), cwd=project, env=env)
                run(migrate, cwd=project, env=runtime_env)
                run(cargo("test", "-p", f"{name}-app-blog", "--features", backend),
                    cwd=project, env=env)
                # The re-export must not shadow the existing app_tasks dependency.
                run(manage("createapp", "app_tasks"), cwd=project, env=env)
                run(manage("makemigrations", "--check"), cwd=project, env=env)
                run(cargo("test"), cwd=project, env=env)

            # Exercise public preview and manual authoring commands with a fresh history.
            sqlite_manage = lambda *options: cargo(
                "run", "--no-default-features", "--features", "sqlite", "--bin", "manage", "--", *options,
            )
            run(manage("makemigrations", "--empty", "tasks", "--name", "backfill"), cwd=project, env=env)
            manual_file, = migration_dir.glob("m*_backfill.rs")
            manual_name = manual_file.stem[1:]
            if backend == "sqlite" and modular:
                # Exercise stdout as a single script, including a statement whose
                # last line is a comment. Edit the freshly generated empty file.
                statements = [
                    "CREATE VIEW first_preview AS SELECT 42 AS id -- published;",
                    "CREATE VIEW outer_preview AS SELECT id FROM first_preview",
                ]
                operations = ", ".join(
                    "MigrationOperation::RunSql { forward: " + json.dumps(sql) +
                    ".into(), reverse: None, state_operations: vec![] }" for sql in statements
                )
                source = manual_file.read_text()
                before, marker, after = source.rpartition("vec![]")
                assert marker
                manual_file.write_text(before + "vec![" + operations + "]" + after)
                script = subprocess.check_output(manage("sqlmigrate", "tasks", manual_name),
                                                 cwd=project, env=env, text=True)
                with sqlite3.connect(":memory:") as db:
                    db.executescript(script)
                    for view in ("first_preview", "outer_preview"):
                        assert db.execute(f"SELECT id FROM {view}").fetchone() == (42,)
            preview = subprocess.check_output(sqlite_manage("migrate", "tasks", manual_name, "--plan"),
                                              cwd=project, env=runtime_env, text=True)
            assert manual_name in preview
            sql = subprocess.check_output(manage("sqlmigrate", "tasks", "0001_initial"),
                                          cwd=project, env=env, text=True)
            assert "CREATE TABLE" in sql and "tasks_task" in sql
            run(sqlite_manage("migrate", "tasks", manual_name), cwd=project, env=runtime_env)
            run(sqlite_manage("migrate", "status"), cwd=project, env=runtime_env)
            if backend == "sqlite" and not modular:
                # Simulate importing an unapplied file authored on the other Git branch.
                previous = max(f.stem[1:] for f in migration_dir.glob("m[0-9]*.rs")
                               if f != manual_file)
                run(manage("makemigrations", "--empty", "--name", "branch"), cwd=project, env=env)
                branch, = migration_dir.glob("m*_branch.rs")
                assert manual_name in branch.read_text()
                branch.write_text(branch.read_text().replace(manual_name, previous))
                expect_failure(sqlite_manage("migrate", "--plan"), cwd=project, env=runtime_env,
                               message="conflicting migration heads")
                run(manage("makemigrations", "--merge"), cwd=project, env=env)
                merge_file, = migration_dir.glob("m*_merge.rs")
                assert all(dep in merge_file.read_text() for dep in (manual_name, branch.stem[1:]))
                run(migrate, cwd=project, env=runtime_env)
                repeated = subprocess.check_output(migrate, cwd=project, env=runtime_env, text=True)
                assert "Applied 0 migration(s)" in repeated
            if modular:
                with model.open("a") as out:
                    out.write('\n#[derive(Debug, Model)]\n#[orm(sync, sync_name = "tasks.note", app = super::APP_NAME)]\n'
                              'pub struct Note { #[orm(primary_key)] pub id: che_orm::Uuid, pub body: String }\n')
                tasks_lib = tasks_src / "lib.rs"
                tasks_lib.write_text(tasks_lib.read_text().replace('.model::<models::Task>()',
                                     '.model::<models::Task>().client_model::<models::Note>()'))
                run(manage("makemigrations", "--name", "sync_note"), cwd=project, env=env)
                assert list((project / "src/che_sync_migrations").glob("m*_sync_note.rs"))
                run(manage("makemigrations", "--check"), cwd=project, env=env)
                run(migrate, cwd=project, env=runtime_env)
            with sqlite3.connect(project / "app.db") as db:
                db.execute("UPDATE che_migration_history SET checksum = 'test mismatch' WHERE app = ? AND name = ?",
                           ("tasks", manual_name))
            expect_failure(sqlite_manage("migrate", "status"), cwd=project, env=runtime_env,
                           message=f"divergent operation migration tasks:{manual_name}")


if __name__ == "__main__":
    main()
