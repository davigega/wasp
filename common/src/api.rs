// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use serde::{Deserialize, Serialize};

/// Response of the `rooms` endpoint.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct Rooms(pub Vec<Room>);

/// Information for one `Room
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct Room {
    pub id: uuid::Uuid,
    pub name: String,
    pub description: Option<String>,
    pub number_of_subscribers: u32,
    #[serde(with = "chrono::serde::ts_seconds")]
    pub creation_date: chrono::DateTime<chrono::Utc>,
}
