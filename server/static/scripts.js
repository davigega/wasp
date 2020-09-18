// reference to the rooms <table> element
const rooms_table = document.getElementById('rooms');

// websocket connection if connected/connecting to a room, otherwise null
var websocket = null;
// UUID of playing room, otherwise null
var playing_room = null;

// TODO: exception handling

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

            rooms.sort((a, b) => a.name.localeCompare(b.name));

            const tbody = document.createElement('tbody');
            var found_playing_room = false;
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
    }
    console.debug('playing ' + id);

    playing_room = id;
    connectWebsocket();

    // TODO: Prepare WebRTC

    const play_row = document.getElementById(id);
    play_row.classList.add('playing');

    const play_button = play_row.cells[4].firstChild;
    play_button.textContent = 'Pause';
}

function pauseRoom() {
    if (playing_room == null) {
        return;
    }

    console.debug('pausing ' + playing_room);

    // TODO: Stop playback

    if (websocket != null) {
        websocket.close();
        websocket = null;
    }

    const play_row = document.getElementById(playing_room);
    play_row.classList.remove('playing');

    const play_button = play_row.cells[4].firstChild;
    play_button.textContent = 'Play';

    playing_room = null;
}

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

    ws.addEventListener('open', (event) => onServerOpen(ws, event));
    ws.addEventListener('error', (event) => onServerError(ws, event));
    ws.addEventListener('message', (event) => onServerMessage(ws, event));
    ws.addEventListener('close', (event) => onServerClose(ws, event));

    websocket = ws;
}

function onServerOpen(ws, event) {
    // Ignore if the situation changed in the meantime
    if (playing_room == null || ws != websocket) {
        return;
    }

    console.log('Opened');

    // Now join the room, at this point we will start getting messages
    websocket.send(JSON.stringify({
        'joinroom': {
            'id': playing_room
        }
    }));
}

function onServerMessage(ws, event) {
    // Ignore if the situation changed in the meantime
    if (playing_room == null || ws != websocket) {
        return;
    }

    console.log('Received ' + event.data);

    const msg = JSON.parse(event.data);
    console.log(msg);
}

function onServerClose(ws, event) {
    // Ignore if the situation changed in the meantime
    if (playing_room == null || ws != websocket) {
        return;
    }

    console.log('Disconnected');

    websocket = null;
    pauseRoom();
}

function onServerError(ws, event) {
    // Ignore if the situation changed in the meantime
    if (playing_room == null || ws != websocket) {
        return;
    }

    console.log('Server error ' + event.data);

    websocket.close();
    websocket = null;
    pauseRoom();
}
