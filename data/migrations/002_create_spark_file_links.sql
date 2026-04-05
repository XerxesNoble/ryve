-- Spark-file links: associate sparks with file regions
CREATE TABLE IF NOT EXISTS spark_file_links (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    spark_id     TEXT NOT NULL REFERENCES sparks(id) ON DELETE CASCADE,
    file_path    TEXT NOT NULL,
    line_start   INTEGER,
    line_end     INTEGER,
    workshop_id  TEXT NOT NULL,
    created_at   TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_spark_file_links_spark ON spark_file_links(spark_id);
CREATE INDEX IF NOT EXISTS idx_spark_file_links_file ON spark_file_links(file_path, workshop_id);
