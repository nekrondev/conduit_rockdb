use crate::{database::DatabaseGuard, Error};
use ruma::{
    api::{client::r0::uiaa::UiaaResponse, OutgoingResponse},
    identifiers::{DeviceId, UserId},
    signatures::CanonicalJsonValue,
    Outgoing, ServerName,
};
use std::ops::Deref;

#[cfg(feature = "conduit_bin")]
use {
    crate::server_server,
    rocket::{
        data::{self, ByteUnit, Data, FromData},
        http::Status,
        outcome::Outcome::*,
        response::{self, Responder},
        tokio::io::AsyncReadExt,
        Request,
    },
    ruma::api::{AuthScheme, IncomingRequest},
    std::collections::BTreeMap,
    std::io::Cursor,
    tracing::{debug, warn},
};

/// This struct converts rocket requests into ruma structs by converting them into http requests
/// first.
pub struct Ruma<T: Outgoing> {
    pub body: T::Incoming,
    pub sender_user: Option<Box<UserId>>,
    pub sender_device: Option<Box<DeviceId>>,
    pub sender_servername: Option<Box<ServerName>>,
    // This is None when body is not a valid string
    pub json_body: Option<CanonicalJsonValue>,
    pub from_appservice: bool,
}

#[cfg(feature = "conduit_bin")]
#[rocket::async_trait]
impl<'a, T: Outgoing> FromData<'a> for Ruma<T>
where
    T::Incoming: IncomingRequest,
{
    type Error = ();

    #[tracing::instrument(skip(request, data))]
    async fn from_data(
        request: &'a Request<'_>,
        data: Data<'a>,
    ) -> data::Outcome<'a, Self, Self::Error> {
        let metadata = T::Incoming::METADATA;
        let db = request
            .guard::<DatabaseGuard>()
            .await
            .expect("database was loaded");

        // Get token from header or query value
        let token = request
            .headers()
            .get_one("Authorization")
            .and_then(|s| s.get(7..)) // Split off "Bearer "
            .or_else(|| request.query_value("access_token").and_then(|r| r.ok()));

        let limit = db.globals.max_request_size();
        let mut handle = data.open(ByteUnit::Byte(limit.into()));
        let mut body = Vec::new();
        if handle.read_to_end(&mut body).await.is_err() {
            // Client disconnected
            // Missing Token
            return Failure((Status::new(582), ()));
        }

        let mut json_body = serde_json::from_slice::<CanonicalJsonValue>(&body).ok();

        let (sender_user, sender_device, sender_servername, from_appservice) = if let Some((
            _id,
            registration,
        )) = db
            .appservice
            .all()
            .unwrap()
            .iter()
            .find(|(_id, registration)| {
                registration
                    .get("as_token")
                    .and_then(|as_token| as_token.as_str())
                    .map_or(false, |as_token| token == Some(as_token))
            }) {
            match metadata.authentication {
                AuthScheme::AccessToken | AuthScheme::QueryOnlyAccessToken => {
                    let user_id = request.query_value::<String>("user_id").map_or_else(
                        || {
                            UserId::parse_with_server_name(
                                registration
                                    .get("sender_localpart")
                                    .unwrap()
                                    .as_str()
                                    .unwrap(),
                                db.globals.server_name(),
                            )
                            .unwrap()
                        },
                        |string| {
                            UserId::parse(string.expect("parsing to string always works")).unwrap()
                        },
                    );

                    if !db.users.exists(&user_id).unwrap() {
                        // Forbidden
                        return Failure((Status::new(580), ()));
                    }

                    // TODO: Check if appservice is allowed to be that user
                    (Some(user_id), None, None, true)
                }
                AuthScheme::ServerSignatures => (None, None, None, true),
                AuthScheme::None => (None, None, None, true),
            }
        } else {
            match metadata.authentication {
                AuthScheme::AccessToken | AuthScheme::QueryOnlyAccessToken => {
                    if let Some(token) = token {
                        match db.users.find_from_token(token).unwrap() {
                            // Unknown Token
                            None => return Failure((Status::new(581), ())),
                            Some((user_id, device_id)) => (
                                Some(user_id),
                                Some(Box::<DeviceId>::from(device_id)),
                                None,
                                false,
                            ),
                        }
                    } else {
                        // Missing Token
                        return Failure((Status::new(582), ()));
                    }
                }
                AuthScheme::ServerSignatures => {
                    // Get origin from header
                    let x_matrix = match request
                        .headers()
                        .get_one("Authorization")
                        .and_then(|s| s.get(9..)) // Split off "X-Matrix " and parse the rest
                        .map(|s| {
                            s.split_terminator(',')
                                .map(|field| {
                                    let mut splits = field.splitn(2, '=');
                                    (splits.next(), splits.next().map(|s| s.trim_matches('"')))
                                })
                                .collect::<BTreeMap<_, _>>()
                        }) {
                        Some(t) => t,
                        None => {
                            warn!("No Authorization header");

                            // Forbidden
                            return Failure((Status::new(580), ()));
                        }
                    };

                    let origin_str = match x_matrix.get(&Some("origin")) {
                        Some(Some(o)) => *o,
                        _ => {
                            warn!("Invalid X-Matrix header origin field: {:?}", x_matrix);

                            // Forbidden
                            return Failure((Status::new(580), ()));
                        }
                    };

                    let origin = match ServerName::parse(origin_str) {
                        Ok(s) => s,
                        _ => {
                            warn!(
                                "Invalid server name in X-Matrix header origin field: {:?}",
                                x_matrix
                            );

                            // Forbidden
                            return Failure((Status::new(580), ()));
                        }
                    };

                    let key = match x_matrix.get(&Some("key")) {
                        Some(Some(k)) => *k,
                        _ => {
                            warn!("Invalid X-Matrix header key field: {:?}", x_matrix);

                            // Forbidden
                            return Failure((Status::new(580), ()));
                        }
                    };

                    let sig = match x_matrix.get(&Some("sig")) {
                        Some(Some(s)) => *s,
                        _ => {
                            warn!("Invalid X-Matrix header sig field: {:?}", x_matrix);

                            // Forbidden
                            return Failure((Status::new(580), ()));
                        }
                    };

                    let mut request_map = BTreeMap::<String, CanonicalJsonValue>::new();

                    if let Some(json_body) = &json_body {
                        request_map.insert("content".to_owned(), json_body.clone());
                    };

                    request_map.insert(
                        "method".to_owned(),
                        CanonicalJsonValue::String(request.method().to_string()),
                    );
                    request_map.insert(
                        "uri".to_owned(),
                        CanonicalJsonValue::String(request.uri().to_string()),
                    );
                    request_map.insert(
                        "origin".to_owned(),
                        CanonicalJsonValue::String(origin.as_str().to_owned()),
                    );
                    request_map.insert(
                        "destination".to_owned(),
                        CanonicalJsonValue::String(db.globals.server_name().as_str().to_owned()),
                    );

                    let mut origin_signatures = BTreeMap::new();
                    origin_signatures
                        .insert(key.to_owned(), CanonicalJsonValue::String(sig.to_owned()));

                    let mut signatures = BTreeMap::new();
                    signatures.insert(
                        origin.as_str().to_owned(),
                        CanonicalJsonValue::Object(origin_signatures),
                    );

                    request_map.insert(
                        "signatures".to_owned(),
                        CanonicalJsonValue::Object(signatures),
                    );

                    let keys =
                        match server_server::fetch_signing_keys(&db, &origin, vec![key.to_owned()])
                            .await
                        {
                            Ok(b) => b,
                            Err(e) => {
                                warn!("Failed to fetch signing keys: {}", e);

                                // Forbidden
                                return Failure((Status::new(580), ()));
                            }
                        };

                    let mut pub_key_map = BTreeMap::new();
                    pub_key_map.insert(origin.as_str().to_owned(), keys);

                    match ruma::signatures::verify_json(&pub_key_map, &request_map) {
                        Ok(()) => (None, None, Some(origin), false),
                        Err(e) => {
                            warn!(
                                "Failed to verify json request from {}: {}\n{:?}",
                                origin, e, request_map
                            );

                            if request.uri().to_string().contains('@') {
                                warn!("Request uri contained '@' character. Make sure your reverse proxy gives Conduit the raw uri (apache: use nocanon)");
                            }

                            // Forbidden
                            return Failure((Status::new(580), ()));
                        }
                    }
                }
                AuthScheme::None => (None, None, None, false),
            }
        };

        let mut http_request = http::Request::builder()
            .uri(request.uri().to_string())
            .method(&*request.method().to_string());
        for header in request.headers().iter() {
            http_request = http_request.header(header.name.as_str(), &*header.value);
        }

        if let Some(json_body) = json_body.as_mut().and_then(|val| val.as_object_mut()) {
            let user_id = sender_user.clone().unwrap_or_else(|| {
                UserId::parse_with_server_name("", db.globals.server_name())
                    .expect("we know this is valid")
            });

            if let Some(CanonicalJsonValue::Object(initial_request)) = json_body
                .get("auth")
                .and_then(|auth| auth.as_object())
                .and_then(|auth| auth.get("session"))
                .and_then(|session| session.as_str())
                .and_then(|session| {
                    db.uiaa
                        .get_uiaa_request(
                            &user_id,
                            &sender_device.clone().unwrap_or_else(|| "".into()),
                            session,
                        )
                        .ok()
                        .flatten()
                })
            {
                for (key, value) in initial_request {
                    json_body.entry(key).or_insert(value);
                }
            }
            body = serde_json::to_vec(json_body).expect("value to bytes can't fail");
        }

        let http_request = http_request.body(&*body).unwrap();
        debug!("{:?}", http_request);
        match <T::Incoming as IncomingRequest>::try_from_http_request(http_request) {
            Ok(t) => Success(Ruma {
                body: t,
                sender_user,
                sender_device,
                sender_servername,
                from_appservice,
                json_body,
            }),
            Err(e) => {
                warn!("{:?}", e);
                // Bad Json
                Failure((Status::new(583), ()))
            }
        }
    }
}

impl<T: Outgoing> Deref for Ruma<T> {
    type Target = T::Incoming;

    fn deref(&self) -> &Self::Target {
        &self.body
    }
}

/// This struct converts ruma responses into rocket http responses.
pub type ConduitResult<T> = Result<RumaResponse<T>, Error>;

pub fn response<T: OutgoingResponse>(response: RumaResponse<T>) -> response::Result<'static> {
    let http_response = response
        .0
        .try_into_http_response::<Vec<u8>>()
        .map_err(|_| Status::InternalServerError)?;

    let mut response = rocket::response::Response::build();

    let status = http_response.status();
    response.status(Status::new(status.as_u16()));

    for header in http_response.headers() {
        response.raw_header(header.0.to_string(), header.1.to_str().unwrap().to_owned());
    }

    let http_body = http_response.into_body();

    response.sized_body(http_body.len(), Cursor::new(http_body));

    response.raw_header("Access-Control-Allow-Origin", "*");
    response.raw_header(
        "Access-Control-Allow-Methods",
        "GET, POST, PUT, DELETE, OPTIONS",
    );
    response.raw_header(
        "Access-Control-Allow-Headers",
        "Origin, X-Requested-With, Content-Type, Accept, Authorization",
    );
    response.raw_header("Access-Control-Max-Age", "86400");
    response.ok()
}

#[derive(Clone)]
pub struct RumaResponse<T>(pub T);

impl<T> From<T> for RumaResponse<T> {
    fn from(t: T) -> Self {
        Self(t)
    }
}

impl From<Error> for RumaResponse<UiaaResponse> {
    fn from(t: Error) -> Self {
        t.to_response()
    }
}

#[cfg(feature = "conduit_bin")]
impl<'r, 'o, T> Responder<'r, 'o> for RumaResponse<T>
where
    'o: 'r,
    T: OutgoingResponse,
{
    fn respond_to(self, _: &'r Request<'_>) -> response::Result<'o> {
        response(self)
    }
}
