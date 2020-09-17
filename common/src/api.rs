// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use serde::{Deserialize, Serialize};

/// Response of the `rooms` endpoint.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct Rooms(Vec<Room>);

/// Information for one `Room
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct Room {
    id: uuid::Uuid,
    name: String,
    description: Option<String>,
}
