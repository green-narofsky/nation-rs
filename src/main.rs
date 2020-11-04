use structopt::StructOpt;
// use std::time::{Instant, Duration};
use chrono::{DateTime, Duration, Utc};
use core::convert::Infallible;
use core::str::FromStr;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::fmt::Debug;
use thiserror::Error;

/// Base URL of the NationStates API.
const API_BASE: &'static str = "https://www.nationstates.net/cgi-bin/api.cgi";
/// The NationStates API version this library is written against.
const API_VERSION: u16 = 11;

/// Session pin for the NationStates API.
#[derive(Serialize, Deserialize, Debug)]
pub struct Pin {
    value: u64,
    timestamp: DateTime<Utc>,
}
impl Pin {
    /// Check validity based on timestamp.
    /// Note that pins are also invalidated by additional logins.
    fn valid(&self) -> bool {
        Utc::now().signed_duration_since(self.timestamp) < Duration::hours(2)
    }
}

/// Authentication information for the NationStates API.
// A usable `Auth` will have at least one `Some` in its fields.
#[derive(Serialize, Deserialize)]
struct Auth {
    // Storage should prefer storing autologin tokens over passwords.
    password: Option<String>,
    autologin: Option<String>,
    pin: Option<Pin>,
}
impl Default for Auth {
    fn default() -> Self {
        Self {
            password: None,
            autologin: None,
            pin: None,
        }
    }
}
impl Debug for Auth {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "Auth")
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct Nation {
    name: String,
    auth: Auth,
}
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename = "nations")]
struct Nations {
    #[serde(rename(deserialize = "$value", serialize = "nation"))]
    inner: Vec<Nation>,
}
impl Nations {
    /// Make a new collection of `Nation`s.
    fn new() -> Self {
        Self { inner: Vec::new() }
    }
}

#[derive(StructOpt)]
struct ProfilePath {
    path: PathBuf,
}
impl Default for ProfilePath {
    fn default() -> Self {
        // Separated out so I can do platform specific stuff if I want.
        use directories::ProjectDirs;
        let proj_dirs = ProjectDirs::from("", "", "Nation").unwrap();
        Self {
            path: proj_dirs.data_dir().join("nation.xml"),
        }
    }
}
impl FromStr for ProfilePath {
    type Err = Infallible;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Ok(Self {
            path: PathBuf::from(input),
        })
    }
}
impl ToString for ProfilePath {
    fn to_string(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

// TODO: Consider separating the manually authored
// profile and cached data retrieved from the API
// into two separate files.
// This is low priority because no
// customization options come to mind.
#[derive(Serialize, Debug)]
struct Profile {
    nations: Nations,
}
#[derive(Error, Debug)]
enum ProfileError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("xml error: {0}")]
    XmlError(#[from] quick_xml::DeError),
}
impl Profile {
    fn load(path: &Path) -> Result<Self, ProfileError> {
        let file = match std::fs::File::open(&path).map_err(|e| (e.kind(), e)) {
            Ok(f) => f,
            Err((std::io::ErrorKind::NotFound, _)) => return Ok(Self::default()),
            Err((_, e)) => Err(e)?,
        };
        let reader = std::io::BufReader::new(file);
        let nations = quick_xml::de::from_reader(reader)?;
        Ok(Self { nations })
    }
    fn save(&self, path: &Path) -> Result<(), ProfileError> {
        let writer = std::fs::File::create(&path)?;
        Ok(quick_xml::se::to_writer(writer, &self.nations)?)
    }
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            nations: Nations::new(),
        }
    }
}

#[derive(StructOpt)]
enum Opt {
    /// Ping nation(s)
    Ping {
        #[structopt(short, long, default_value)]
        profile: ProfilePath,
        /// Retry with autologin or password if pin authentication fails
        #[structopt(short)]
        retry_pin: bool,
        /// Name of the nation to ping
        nation: String,
    },
    /// Add a nation to profile
    Add {
        #[structopt(short, long, default_value)]
        profile: ProfilePath,
        name: String,
        password: String,
    },
    #[allow(dead_code)]
    /// Save new password for a nation
    NewPassword {
        #[structopt(short, long, default_value)]
        profile: ProfilePath,
        /// Name of the nation whose password has changed
        nation: String,
        /// New password for this nation
        password: String,
    }
}

mod api {
    use std::borrow::Cow;
    use itertools::Itertools;
    use super::{Auth, Pin};
    use chrono::Utc;
    use serde::Deserialize;
    use reqwest::StatusCode;
    #[derive(Debug)]
    pub enum Shard {
        Ping,
    }
    impl Shard {
        fn to_query_segment(&self) -> Cow<'_, str> {
            // This may end up generated.
            match self {
                Shard::Ping => "ping".into(),
            }
        }
    }
    fn query_string(shards: &[Shard]) -> String {
        shards.into_iter().map(Shard::to_query_segment).join("+")
    }
    #[derive(Debug, Deserialize)]
    enum ResolvedShard {
        #[serde(rename(deserialize = "PING"))]
        Ping,
    }
    #[derive(Debug)]
    pub struct Request<'a> {
        pub(crate) nation: &'a super::Nation,
        pub(crate) shards: Vec<Shard>,
    }
    impl Request<'_> {
        // There are a bunch of copies and allocations
        // involved in building this string,
        // but it's not an optimization priority.
        // LLVM probably sees through them anyway.
        pub fn url(&self) -> String {
            let mut res = String::from(crate::API_BASE);
            res.push_str("?nation=");
            res.push_str(&self.nation.name);
            res.push_str("&q=");
            res.push_str(&query_string(&self.shards));
            res.push_str("&v=");
            res.push_str(&crate::API_VERSION.to_string());
            res
        }
    }
    #[derive(Debug, Deserialize)]
    pub struct NationData {
        #[serde(rename(deserialize = "$value"))]
        inner: Vec<ResolvedShard>,
    }
    #[derive(Debug)]
    #[non_exhaustive]
    pub struct Response {
        pub data: NationData,
        pub autologin: Option<String>,
        pub pin: Option<Pin>,
    }
    #[derive(Debug)]
    pub enum Failure {
        NoAuth,
        BadAuth,
        // Bad pins are special because pins expire,
        // so this is potentially recoverable.
        // Also, pins can be invalidated by logging in separately.
        // The `.valid()` method on pins is likely to
        // handle pin expiration, but not arbitrary pin invalidation.
        BadPin,
        Other(StatusCode),
    }
    impl Request<'_> {
        pub async fn send(&self, client: &reqwest::Client) -> Result<Response, Failure> {
            // `reqwest` is on Tokio 0.2 still. We're on Tokio 0.3.
            use tokio_compat_02::FutureExt;
            let mut request = client.get(&self.url());
            let mut using_pin = false;
            match &self.nation.auth {
                // Note that pins fail more easily than autologins or passwords.
                // If a pin fails and we have another credential on hand,
                // we should retry and save the pin we get next.
                // This method won't control that behavior, though.
                // It will simply return a distinct error code for that case.
                Auth { pin: Some(pin), .. } if pin.valid() => {
                    request = request.header("X-Pin", pin.value);
                    using_pin = true;
                },
                Auth { autologin: Some(autologin), .. } => {
                    request = request.header("X-Autologin", autologin);
                },
                Auth { password: Some(password), .. } => {
                    request = request.header("X-Password", password);
                },
                _ => return Err(Failure::NoAuth),
            };
            let response = request.send().compat().await.unwrap();
            let timestamp = Utc::now();
            let headers = response.headers();
            let (pin_value, autologin) = (headers.get("X-Pin")
                                          .and_then(|x| x.to_str().ok()?.parse().ok()),
                                          headers.get("X-Autologin")
                                          .and_then(|x| x.to_str().ok().map(String::from)));
            let pin = pin_value.map(|value| Pin {
                value, timestamp,
            });
            let status = response.status();
            if status == StatusCode::OK {
                let text = response.text().await.unwrap();
                // println!("Response text: {}", text);
                let data = quick_xml::de::from_str(&text).unwrap();
                println!("Using pin: {}", using_pin);
                Ok(Response { data, autologin, pin })
            } else {
                Err(if status == StatusCode::FORBIDDEN {
                    if using_pin { Failure::BadPin } else { Failure::BadAuth }
                } else {
                    Failure::Other(status)
                })
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opt = Opt::from_args();
    // println!("timestamp: {}", quick_xml::se::to_string(&Utc::now()).unwrap());
    match opt {
        Opt::Ping { profile: profile_path, nation, retry_pin } => {
            let mut profile = Profile::load(&profile_path.path)?;
            // println!("Profile: {:#?}", profile);
            // println!("XML Profile: {}", quick_xml::se::to_string(&profile.nations).unwrap());
            let nation = match profile.nations.inner.iter_mut().find(|x| x.name == nation) {
                Some(x) => x,
                None => anyhow::bail!("Nation {} not found.", nation),
            };
            let req = api::Request {
                shards: vec![api::Shard::Ping],
                nation,
            };
            println!("Request: {:?}", req);
            println!("Request URL: {}", req.url());
            let client = reqwest::Client::builder()
                .user_agent("nation-rs/0.0.0 7ytd765789@gmail.com").build().unwrap();
            let res = req.send(&client).await;
            match res {
                Ok(api::Response { data, autologin, pin }) => {
                    println!("Ok: {:?}", data);
                    if let Some(autologin) = autologin {
                        nation.auth.autologin = Some(autologin);
                        // Since autologins last as long as passwords do,
                        // we can delete our stored password.
                        nation.auth.password = None;
                    }
                    if let Some(pin) = pin {
                        nation.auth.pin = Some(pin);
                    }
                    profile.save(&profile_path.path)?;
                },
                Err(api::Failure::BadPin) => {
                    let shards = req.shards;
                    nation.auth.pin = None;
                    if retry_pin {
                        let res = api::Request {
                            shards, nation,
                        }.send(&client).await;
                        match res {
                            Ok(api::Response { data, autologin, pin }) => {
                                println!("Result: {:?}", data);
                                if let Some(autologin) = autologin {
                                    nation.auth.autologin = Some(autologin);
                                    // Since autologins last as long as passwords do,
                                    // we can delete our stored password.
                                    nation.auth.password = None;
                                }
                                if let Some(pin) = pin {
                                    nation.auth.pin = Some(pin);
                                }
                                profile.save(&profile_path.path)?;
                            },
                            Err(e) => anyhow::bail!("Failure: {:?}", e),
                        }
                    }
                },
                Err(e) => anyhow::bail!("Failure: {:?}", e),
            }
        }
        #[allow(unused_variables)]
        Opt::Add {
            profile,
            name,
            password,
        } => todo!("adding nations to profile on command line"),
        Opt::NewPassword { .. } => todo!("password changes"),
    }

    Ok(())
}
