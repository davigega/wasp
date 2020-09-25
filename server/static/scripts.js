// reference to the rooms <table> element
const rooms_table = document.getElementById('rooms');

// reference to the player <audio> element
const audio_player = document.getElementById('audio');

// Override with your own STUN/TURN servers if you want
const rtc_config = {iceServers: [{urls: 'stun:stun.l.google.com:19302'}]};

// websocket connection if connected/connecting to a room, otherwise null
var websocket = null;
// UUID of playing room, otherwise null
var playing_room = null;
// Peer connection if currently playing a room, otherwise null
var peer_connection = null;

// TODO: exception handling

function onLoad() {
    updateRooms();
    setInterval(updateRooms, 5000);
}

function updateRooms() {
    // Configure a different API URL here if desired
    const api_url = '/api/rooms';

    var request = new XMLHttpRequest();
    request.open('GET', api_url, true);
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

    playing_room = id;
    connectWebsocket();

    const play_row = document.getElementById(id);
    play_row.classList.add('playing');

    const play_button = play_row.cells[4].firstChild;
    play_button.textContent = 'Pause';

    const listener_count = play_row.cells[3].firstChild;
    current_listener_count = parseInt(listener_count.textContent);
    listener_count.textContent = current_listener_count + 1;
}

function pauseRoom() {
    if (playing_room == null) {
        return;
    }

    audio_player.srcObject = null;

    if (peer_connection != null) {
        peer_connection.close();
        peer_connection = null;
    }

    if (websocket != null) {
        websocket.close();
        websocket = null;
    }

    const play_row = document.getElementById(playing_room);
    play_row.classList.remove('playing');

    const play_button = play_row.cells[4].firstChild;
    play_button.textContent = 'Play';

    const listener_count = play_row.cells[3].firstChild;
    current_listener_count = parseInt(listener_count.textContent);
    if (current_listener_count > 0) {
        listener_count.textContent = current_listener_count - 1;
    }

    playing_room = null;
}

function getWebSocketUrl() {
    var scheme;
    var port;

    // Configure a different WebSocket server here if desired

    if (window.location.protocol == 'https:') {
        scheme = 'wss:';
        port = 443;
    } else if (window.location.protocol == 'http:') {
        scheme = 'ws:';
        port = 80;
    }

    const server = window.location.hostname;
    port = window.location.port || port;
    const ws_url = scheme + '//' + server + ':' + port + '/ws/subscribe';

    return ws_url;
}

function connectWebsocket() {
    const ws = new WebSocket(getWebSocketUrl());

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

    // Now join the room, at this point we will start getting messages
    websocket.send(JSON.stringify({
        'joinroom': {
            'id': playing_room
        }
    }));

    peer_connection = new RTCPeerConnection(rtc_config);
    peer_connection.onicecandidate = function(event) {
        if (websocket == null || event.candidate == null) {
            return;
        }

        websocket.send(JSON.stringify({'ice': event.candidate }));
    };

    peer_connection.ontrack = function(event) {
        audio_player.srcObject = event.streams[0];
    };
}

function onServerMessage(ws, event) {
    // Ignore if the situation changed in the meantime
    if (playing_room == null || ws != websocket) {
        return;
    }

    const msg = JSON.parse(event.data);

    if (msg.sdp != null) {
        if (peer_connection == null) {
            return;
        }

        peer_connection.setRemoteDescription(msg.sdp).then(function() {
            peer_connection.createAnswer().then(function(sdp) {
                peer_connection.setLocalDescription(sdp).then(function() {
                    if (websocket != null) {
                        websocket.send(JSON.stringify({'sdp': peer_connection.localDescription }));
                    }
                });
            });
        });
    } else if (msg.ice != null) {
        if (peer_connection == null) {
            return;
        }
        peer_connection.addIceCandidate(new RTCIceCandidate(msg.ice));
    } else if (msg.error != null) {
        console.error('Got error: ' + msg.error.message);
        pauseRoom();
    } else {
        console.error('Unknown message: ' + msg);
    }
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

    console.error('Server error');

    websocket.close();
    websocket = null;
    pauseRoom();
}
