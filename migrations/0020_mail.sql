-- Mail: feste Zeilennummer für die Volltextsuche, Papierkorb-Zeitpunkt, vom Benutzer gesetzte
-- Ordnerrollen und Indizes für Threading und Abgleich.

-- `num` ist die rowid der Nachricht in `messages_fts`. Die rowid von `messages` selbst taugt
-- dafür nicht (TEXT-Primärschlüssel – VACUUM darf sie neu vergeben).
ALTER TABLE messages ADD COLUMN num INTEGER;
-- Seit wann die Nachricht im Papierkorb liegt (für `trash_retention_days`)
ALTER TABLE messages ADD COLUMN trashed_at INTEGER;
CREATE UNIQUE INDEX messages_num ON messages(num);
CREATE INDEX messages_in_reply_to ON messages(in_reply_to) WHERE in_reply_to <> '';
CREATE INDEX messages_pending ON messages(account_id, sha256) WHERE uid IS NULL;

-- Jede neue Nachricht bekommt die nächste Nummer.
CREATE TRIGGER messages_num_after_insert AFTER INSERT ON messages WHEN new.num IS NULL
BEGIN
    UPDATE messages SET num = (SELECT COALESCE(MAX(num), 0) + 1 FROM messages) WHERE rowid = new.rowid;
END;

-- Gelöschte Nachrichten (auch über ON DELETE CASCADE) verschwinden aus dem Suchindex.
CREATE TRIGGER messages_fts_after_delete AFTER DELETE ON messages
BEGIN
    DELETE FROM messages_fts WHERE rowid = old.num;
END;

-- 1 = Rolle vom Benutzer gesetzt, der Abgleich ändert sie nicht mehr.
ALTER TABLE folders ADD COLUMN role_locked INTEGER NOT NULL DEFAULT 0;
