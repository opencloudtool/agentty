CREATE TABLE project_setting (
    project_id INTEGER NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (project_id, name)
);

INSERT INTO project_setting (project_id, name, value)
SELECT project.id, setting.name, setting.value
FROM project
CROSS JOIN setting
WHERE setting.name != 'ActiveProjectId';
