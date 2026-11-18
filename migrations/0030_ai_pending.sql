-- KI: neue Nachrichten, die die Einteilung im Hintergrund noch nicht gesehen hat, schnell finden.
CREATE INDEX messages_ai_pending ON messages(date DESC)
    WHERE ai_categorized_at IS NULL AND category_source = 'auto';
