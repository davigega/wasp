// Copyright (C) 2020 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the MIT license, see the LICENSE file or <http://opensource.org/licenses/MIT>

use crate::rooms::{self, Rooms};

use actix::Addr;
use actix_web::{web, HttpResponse};

use log::{error, trace};

use webrtc_audio_publishing::api;

pub async fn rooms(rooms: web::Data<Addr<Rooms>>) -> Result<HttpResponse, actix_web::Error> {
    let room_addrs = rooms.send(rooms::ListRoomsMessage).await.map_err(|err| {
        error!("Failed to list rooms: {}", err);
        HttpResponse::InternalServerError()
    })?;

    let mut room_descs = Vec::with_capacity(room_addrs.len());
    for room in room_addrs {
        let room_desc = match room.send(rooms::RoomInformationMessage).await {
            Err(err) => {
                error!(
                    "Failed to retrieve room information for {:?}: {:?}",
                    room, err
                );
                continue;
            }
            Ok(desc) => desc,
        };

        room_descs.push(api::Room {
            id: room_desc.id.0,
            name: room_desc.name.clone(),
            description: room_desc.description.clone(),
        });
    }

    trace!("Returning room descriptions {:?}", room_descs);

    Ok(HttpResponse::Ok().json(api::Rooms(room_descs)))
}
