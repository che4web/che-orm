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
            model = project / "src" / ("models.rs" if layout == "single" else "apps/tasks/models.rs")
            assert model.exists()
            assert '#[orm(app = super::APP_NAME, index("title"))]' in model.read_text()
            assert (project / "src/models.rs").exists() == (layout == "single")
            assert (project / "src/apps/tasks").exists() == (layout == "modular")
            migration_dir = project / "src" / "migrations"
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
            run(manage("createapp", "blog"), cwd=project, env=env)
            blog = project / "src/apps/blog"
            assert (blog / "mod.rs").exists() and (blog / "models.rs").exists()
            assert 'pub const APP_NAME: &str = "blog";' in (blog / "mod.rs").read_text()
            assert '#[orm(app = super::APP_NAME)]' in (blog / "models.rs").read_text()
            expect_failure([str(stale), "createapp", "other"], cwd=project, env=env,
                           message="app registry changed")
            assert not (project / "src/apps/other").exists()
            run(manage("makemigrations", "--check"), cwd=project, env=env)
            before = {p.name: p.read_bytes() for p in blog.iterdir()}
            expect_failure(manage("createapp", "blog"), cwd=project, env=env,
                           message="already registered")
            assert before == {p.name: p.read_bytes() for p in blog.iterdir()}
            (blog / "models.rs").write_text(
                'use che_orm::Model;\nuse crate::models::Task;\n\n'
                '#[derive(Debug, Model)]\n#[orm(app = super::APP_NAME)]\n'
                'pub struct Post {\n    #[orm(primary_key)]\n    pub id: i64,\n'
                '    #[orm(foreign_key = Task, on_delete = "cascade")]\n    pub task_id: i64,\n}\n'
            )
            app_mod = blog / "mod.rs"
            app_mod.write_text(app_mod.read_text().replace(
                "SchemaSet::new()", "SchemaSet::new().model::<models::Post>()",
            ))
            run(["cargo", "fmt"], cwd=project, env=env)
            history = {p.name: p.read_bytes() for p in migration_dir.glob("*.rs") if p.name != "mod.rs"}
            run(manage("makemigrations", "--name", "add_blog"), cwd=project, env=env)
            blog_migration = migration_dir / "m0003_add_blog.rs"
            assert blog_migration.exists() and 'app: "blog"' in blog_migration.read_text()
            assert 'name: "blog_post"' in blog_migration.read_text()
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


if __name__ == "__main__":
    main()
