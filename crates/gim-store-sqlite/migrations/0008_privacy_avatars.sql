-- Profile photos of users (CBOR media row) and the own privacy settings
-- (CBOR, one row). Per-chat settings live in the chats data blob.

ALTER TABLE users ADD COLUMN avatar BLOB;

CREATE TABLE privacy (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    data BLOB NOT NULL
);
