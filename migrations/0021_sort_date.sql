-- Nachrichten mit einem Date-Header in der Zukunft (meist Spam oder falsche Uhren) standen
-- bisher immer ganz oben im Posteingang. Sie bekommen den Zeitpunkt, an dem nrzMail sie
-- übernommen hat; neue Nachrichten prüft `store::sort_date` schon beim Speichern.
UPDATE messages SET date = received_at WHERE date > received_at + 86400;
