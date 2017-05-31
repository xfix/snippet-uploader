use diesel::prelude::*;
use diesel;
use diesel::pg::PgConnection;

use rocket::http::{Cookie, Session};
use rocket::request::{self, FromRequest};
use rocket::{Request, Outcome};

use bcrypt;
use ipnetwork::IpNetwork;

use Connection;
use db::schema::{logins, users};

use std::net::{IpAddr, SocketAddr};

const MIN_PASSWORD_LENGTH: usize = 10;
const BCRYPT_COST: u32 = 10;

error_chain! {
    foreign_links {
        Bcrypt(bcrypt::BcryptError);
        Query(diesel::result::Error);
    }

    errors {
        PasswordTooShort
        PasswordsNotIdentical
        InvalidUserOrPassword
    }

}

#[derive(Copy, Clone)]
pub struct UserId(pub i32);

impl UserId {
    pub fn login(self, session: &mut Session, connection: &PgConnection) -> Result<()> {
        #[derive(Insertable)]
        #[table_name = "sessions"]
        struct NewUid {
            user_id: i32,
        }

        use db::schema::sessions;

        let UserId(id) = self;
        let session_id: i32 = diesel::insert(&NewUid { user_id: id }).into(sessions::table)
            .returning(sessions::dsl::session_id)
            .get_result(connection)?;
        session.set(Cookie::new("session_id", session_id.to_string()));
        Ok(())
    }
}

#[derive(FromForm)]
pub struct RegisterForm {
    pub name: String,
    pub password: String,
    pub repeat_password: String,
}

impl RegisterForm {
    fn check_password(&self) -> Result<()> {
        let password = &self.password;
        if *password != self.repeat_password {
            bail!(ErrorKind::PasswordsNotIdentical);
        }
        if password.len() < MIN_PASSWORD_LENGTH {
            bail!(ErrorKind::PasswordTooShort);
        }
        Ok(())
    }

    pub fn register(&self, connection: &PgConnection) -> Result<UserId> {
        #[derive(Insertable)]
        #[table_name="users"]
        struct NewUser<'a> {
            name: &'a str,
            password: &'a str,
        }

        self.check_password()?;

        let new_user = NewUser {
            name: &self.name,
            password: &bcrypt::hash(&self.password, BCRYPT_COST).unwrap(),
        };
        let id = diesel::insert(&new_user).into(users::table)
            .execute(connection)?;
        Ok(UserId(id as i32))
    }
}

#[derive(Insertable)]
#[table_name="logins"]
struct NewLogin {
    ip: IpNetwork,
    user_id: i32,
    successful: bool,
}

fn log_login(connection: &PgConnection,
             UserId(id): UserId,
             ip_to_insert: IpAddr,
             successful_login: bool)
             -> QueryResult<()> {
    use self::logins;

    diesel::insert(&NewLogin {
                        user_id: id,
                        ip: IpNetwork::new(ip_to_insert,
                                           match ip_to_insert {
                                               IpAddr::V4(_) => 32,
                                               IpAddr::V6(_) => 128,
                                           })
                                .unwrap(),
                        successful: successful_login,
                    }).into(logins::table)
            .execute(connection)?;
    Ok(())
}

#[derive(FromForm)]
pub struct LoginForm {
    pub name: String,
    pub password: String,
}

impl LoginForm {
    pub fn login(&self, connection: &PgConnection, address: SocketAddr) -> Result<UserId> {
        #[derive(Queryable)]
        struct PasswordRow {
            id: i32,
            hashed: String,
        }

        use db::schema::users::dsl::*;

        let row: PasswordRow = users.filter(name.eq(&self.name))
            .select((user_id, password))
            .first(connection)?;

        let successful_login = bcrypt::verify(&self.password, &row.hashed)?;
        let id = UserId(row.id);
        log_login(connection, id, address.ip(), successful_login)?;
        if successful_login {
            Ok(id)
        } else {
            bail!(ErrorKind::InvalidUserOrPassword)
        }
    }
}

impl<'a, 'r> FromRequest<'a, 'r> for UserId {
    type Error = ();

    fn from_request(request: &'a Request<'r>) -> request::Outcome<UserId, ()> {
        let id: Option<i32> = request
            .session()
            .get("session_id")
            .and_then(|cookie| cookie.value().parse().ok());

        match id {
            Some(session) => {
                let connection = match Connection::from_request(request) {
                    Outcome::Success(connection) => connection,
                    Outcome::Failure(f) => return Outcome::Failure(f),
                    Outcome::Forward(f) => return Outcome::Forward(f),
                };

                use db::schema::sessions::dsl::*;

                let result = match sessions
                          .filter(session_id.eq(session))
                          .select(user_id)
                          .first(&*connection) {
                    Ok(result) => result,
                    Err(_) => return Outcome::Forward(()),
                };
                Outcome::Success(UserId(result))
            }
            None => Outcome::Forward(()),
        }
    }
}
