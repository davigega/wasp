const rooms_table = document.getElementById('rooms');
var websocket = connectWebsocket();

var playing_room = null;

function connectWebsocket() {
    var scheme;
    if (window.location.protocol == 'https:') {
        scheme = 'wss:';
    } else if (window.location.protocol == 'http:') {
        scheme = 'ws:';
    }
    const server = window.location.hostname;
    const port = window.location.port || 80;
    const ws_url = scheme + '//' + server + ':' + port + '/ws/subscribe';

    const ws = new WebSocket(ws_url);

    ws.addEventListener('error', onServerError);
    ws.addEventListener('message', onServerMessage);
    ws.addEventListener('close', onServerClose);

    return ws;
}

function onLoad() {
    updateRooms();
    setInterval(updateRooms, 5000);
}

function updateRooms() {
    var request = new XMLHttpRequest();
    request.open('GET', '/api/rooms', true);
    request.onload = function () {
        if (request.status >= 200 && request.status < 400) {
            var rooms = JSON.parse(this.response);

            const tbody = document.createElement('tbody');
            var found_playing_room = false;
            // TODO: Sort rooms by name, include number of listeners and
            // creation date
            rooms.forEach(room => {
                const tr = document.createElement('tr');
                tr.id = room.id;

                const td_name = document.createElement('td');
                td_name.textContent = room.name;
                tr.appendChild(td_name);

                const td_description = document.createElement('td');
                td_description.textContent = room.description;
                tr.appendChild(td_description);

                const td_creation_date = document.createElement('td');
                const date = new Date(0);
                date.setUTCSeconds(room.creation_date);
                td_creation_date.textContent = date.toISOString();
                tr.appendChild(td_creation_date);

                const td_number_of_subscribers = document.createElement('td');
                td_number_of_subscribers.textContent = room.number_of_subscribers;
                tr.appendChild(td_number_of_subscribers);

                const td_play = document.createElement('td');
                const play_button = document.createElement('button');
                play_button.type = 'button';
                play_button.id = 'playButton-' + room.id;
                play_button.className = 'playButton';

                if (playing_room == room.id) {
                    play_button.textContent = 'Pause';
                    tr.classList.add('playing');
                } else {
                    play_button.textContent = 'Play';
                }

                play_button.onclick = function() {
                    if (playing_room == room.id) {
                        pauseRoom();
                        return;
                    }

                    if (playing_room != null) {
                        pauseRoom();
                    }

                    playRoom(room.id);
                };
                td_play.appendChild(play_button);
                tr.appendChild(td_play);

                tbody.appendChild(tr);

                if (playing_room != null && room.id == playing_room) {
                    found_playing_room = true;
                }
            });

            if (playing_room && !found_playing_room) {
                console.debug('playing room disappeared');
                pauseRoom();
            }

            rooms_table.replaceChild(tbody, rooms_table.tBodies[0]);
        }
    }

    request.send();
}

function playRoom(id) {
    if (playing_room != null && playing_room != id) {
        pauseRoom();
        // TODO: Wait until this is actually done
    }
    console.debug('playing ' + id);

    websocket.send(JSON.stringify({
        'joinroom': {
            'id': id
        }
    }));

    const play_button = document.getElementById('playButton-' + id);
    play_button.textContent = 'Pause';

    const play_row = document.getElementById(id);
    play_row.classList.add('playing');

    playing_room = id;
}

function pauseRoom() {
    if (playing_room == null) {
        return;
    }

    console.debug('pausing ' + playing_room);

    // TODO: Stop playback

    websocket.send(JSON.stringify('leaveroom'));

    const play_button = document.getElementById('playButton-' + playing_room);
    play_button.textContent = 'Play';

    const play_row = document.getElementById(playing_room);
    play_row.classList.remove('playing');

    playing_room = null;
}

function onServerMessage(event) {
    console.log("Received " + event.data);

    // TODO: Handle messages

}

function onServerClose(event) {
    console.log("Disconnected");

    pauseRoom();
    window.setTimeout(function() {
        websocket = connectWebsocket();
    }, 1000);
}

function onServerError(event) {
    console.log("server error " + event.data);

    pauseRoom();
    window.setTimeout(function() {
        websocket = connectWebsocket();
    }, 3000);
}
