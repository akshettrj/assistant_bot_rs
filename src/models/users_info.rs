use diesel::prelude::*;

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = crate::schema::users_info)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct UserInfo {
    pub id: i64,
    pub first_name: String,
    pub last_name: Option<String>,
    pub username: Option<String>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::users_info)]
pub struct NewUserInfo<'a> {
    pub id: i64,
    pub first_name: &'a str,
    pub last_name: Option<&'a str>,
    pub username: Option<&'a str>,
}
