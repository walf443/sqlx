use std::env;
use std::fs;
use std::fs::File;
use std::io::prelude::*;

use dotenv::dotenv;

use sqlx::postgres::PgRow;
use sqlx::Connect;
use sqlx::Executor;
use sqlx::PgConnection;
use sqlx::PgPool;
use sqlx::Row;

use structopt::StructOpt;

use anyhow::{anyhow, Context, Result};

const MIGRATION_FOLDER: &'static str = "migrations";

/// Sqlx commandline tool
#[derive(StructOpt, Debug)]
#[structopt(name = "Sqlx")]
enum Opt {
    // #[structopt(subcommand)]
    Migrate(MigrationCommand),
}

/// Simple postgres migrator
#[derive(StructOpt, Debug)]
#[structopt(name = "Sqlx migrator")]
enum MigrationCommand {
    /// Initalizes new migration directory with db create script
    // Init {
    //     // #[structopt(long)]
    //     database_name: String,
    // },

    /// Add new migration with name <timestamp>_<migration_name>.sql
    Add {
        // #[structopt(long)]
        name: String,
    },

    /// Run all migrations
    Run,
}

#[tokio::main]
async fn main() -> Result<()> {
    let opt = Opt::from_args();

    match opt {
        Opt::Migrate(command) => match command {
            // Opt::Init { database_name } => init_migrations(&database_name),
            MigrationCommand::Add { name } => add_migration_file(&name)?,
            MigrationCommand::Run => run_migrations().await?,
        },
    };

    println!("All done!");
    Ok(())
}

// fn init_migrations(db_name: &str) {
//     println!("Initing the migrations so hard! db: {:#?}", db_name);
// }

fn add_migration_file(name: &str) -> Result<()> {
    use chrono::prelude::*;
    use std::path::PathBuf;

    fs::create_dir_all(MIGRATION_FOLDER)?;

    let dt = Utc::now();
    let mut file_name = dt.format("%Y-%m-%d_%H-%M-%S").to_string();
    file_name.push_str("_");
    file_name.push_str(name);
    file_name.push_str(".sql");

    let mut path = PathBuf::new();
    path.push(MIGRATION_FOLDER);
    path.push(&file_name);

    let mut file = File::create(path).context("Failed to create file")?;
    file.write_all(b"-- Add migration script here")
        .context("Could not write to file")?;

    println!("Created migration: '{}'", file_name);
    Ok(())
}

pub struct Migration {
    pub name: String,
    pub sql: String,
}

fn load_migrations() -> Result<Vec<Migration>> {
    let entries = fs::read_dir(&MIGRATION_FOLDER).context("Could not find 'migrations' dir")?;

    let mut migrations = Vec::new();

    for e in entries {
        if let Ok(e) = e {
            if let Ok(meta) = e.metadata() {
                if !meta.is_file() {
                    continue;
                }

                if let Some(ext) = e.path().extension() {
                    if ext != "sql" {
                        println!("Wrong ext: {:?}", ext);
                        continue;
                    }
                } else {
                    continue;
                }

                let mut file = File::open(e.path())
                    .with_context(|| format!("Failed to open: '{:?}'", e.file_name()))?;
                let mut contents = String::new();
                file.read_to_string(&mut contents)
                    .with_context(|| format!("Failed to read: '{:?}'", e.file_name()))?;

                migrations.push(Migration {
                    name: e.file_name().to_str().unwrap().to_string(),
                    sql: contents,
                });
            }
        }
    }

    migrations.sort_by(|a, b| a.name.partial_cmp(&b.name).unwrap());

    Ok(migrations)
}

async fn run_migrations() -> Result<()> {
    dotenv().ok();
    let db_url = env::var("DATABASE_URL").context("Failed to find 'DATABASE_URL'")?;

    check_if_db_exists(&db_url).await?;

    let mut pool = PgPool::new(&db_url)
        .await
        .context("Failed to connect to pool")?;

    create_migration_table(&mut pool).await?;

    let migrations = load_migrations()?;

    for mig in migrations.iter() {
        let mut tx = pool.begin().await?;

        if check_if_applied(&mut tx, &mig.name).await? {
            println!("Already applied migration: '{}'", mig.name);
            continue;
        }
        println!("Applying migration: '{}'", mig.name);

        tx.execute(&*mig.sql)
            .await
            .with_context(|| format!("Failed to run migration {:?}", &mig.name))?;

        save_applied_migration(&mut tx, &mig.name).await?;

        tx.commit().await.context("Failed")?;
    }

    Ok(())
}

async fn check_if_db_exists(db_url: &str) -> Result<()> {
    let split: Vec<&str> = db_url.rsplitn(2, '/').collect();

    if split.len() != 2 {
        return Err(anyhow!("Failed to find database name in connection string"));
    }

    let db_name = split[0];
    let base_url = split[1];

    let mut conn = PgConnection::connect(base_url).await?;

    let result: bool =
        sqlx::query("select exists(SELECT 1 from pg_database WHERE datname = $1) as exists")
            .bind(db_name)
            .try_map(|row: PgRow| row.try_get("exists"))
            .fetch_one(&mut conn)
            .await
            .context("Failed to check if database exists")?;

    if !result {
        println!("Database not found. Creating database: {}", db_name);
        sqlx::query(&format!("CREATE DATABASE {}", db_name))
            .execute(&mut conn)
            .await
            .with_context(|| format!("Failed to create database: {}", db_name))?;
    }

    Ok(())
}

async fn create_migration_table(mut pool: &PgPool) -> Result<()> {
    pool.execute(
        r#"
CREATE TABLE IF NOT EXISTS __migrations (
    migration VARCHAR (255) PRIMARY KEY,
    created TIMESTAMP NOT NULL DEFAULT current_timestamp
);
    "#,
    )
    .await
    .context("Failed to create migration table")?;

    Ok(())
}

async fn check_if_applied(connection: &mut PgConnection, migration: &str) -> Result<bool> {
    let result = sqlx::query(
        "select exists(select migration from __migrations where migration = $1) as exists",
    )
    .bind(migration.to_string())
    .try_map(|row: PgRow| row.try_get("exists"))
    .fetch_one(connection)
    .await
    .context("Failed to check migration table")?;

    Ok(result)
}

async fn save_applied_migration(pool: &mut PgConnection, migration: &str) -> Result<()> {
    sqlx::query("insert into __migrations (migration) values ($1)")
        .bind(migration.to_string())
        .execute(pool)
        .await
        .context("Failed to insert migration")?;

    Ok(())
}