use diesel::prelude::*;

use assistant_bot_rs::{models, schema};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut connection =
        diesel::SqliteConnection::establish("sqlite://assistant_bot_database.sqlite").unwrap();

    let new_user_info = models::NewUserInfo {
        id: 1234,
        first_name: "akshettrj",
        last_name: None,
        username: None,
    };

    dbg!(diesel::insert_into(schema::users_info::table)
        .values(&new_user_info)
        .execute(&mut connection)
        .unwrap());

    Ok(())
}
