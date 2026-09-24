CREATE TABLE widget_tags (widget_id BIGINT NOT NULL REFERENCES widgets (id), tag TEXT NOT NULL);
