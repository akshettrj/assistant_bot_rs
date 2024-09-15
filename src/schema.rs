// @generated automatically by Diesel CLI.

diesel::table! {
    users_info (id) {
        id -> BigInt,
        first_name -> Text,
        last_name -> Nullable<Text>,
        username -> Nullable<Text>,
    }
}
