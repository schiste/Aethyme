DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'aethyme_app') THEN
        CREATE ROLE aethyme_app;
    END IF;
END $$;

CREATE DATABASE aethyme_test;
