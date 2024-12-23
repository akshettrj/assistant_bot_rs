use sea_orm::{ConnectionTrait, Database, DbBackend, DbErr, Statement};

const DATABASE_URL: &str = "sqlite://assistantbot.sqlite.db";
const DATABASE_NAME: &str = "assistantbot_db";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = Database::connect(DATABASE_URL).await?;

    let db = &match db.get_database_backend() {
        sea_orm::DatabaseBackend::Postgres => todo!(),
        sea_orm::DatabaseBackend::Sqlite => {
            println!("Hello here!!");
            db
        }
        _ => todo!(),
    };

    Ok(())
}
