SELECT jsonb_build_object(
 'role', current_user, 'session_role', session_user,
 'superuser', r.rolsuper, 'createrole', r.rolcreaterole, 'createdb', r.rolcreatedb,
 'inherit', r.rolinherit, 'replication', r.rolreplication, 'bypassrls', r.rolbypassrls,
 'database_create', has_database_privilege(current_user,current_database(),'CREATE'),
 'database_temp', has_database_privilege(current_user,current_database(),'TEMP'),
 'schema_create', EXISTS(SELECT 1 FROM pg_namespace n WHERE n.nspname NOT LIKE 'pg_%'
   AND n.nspname <> 'information_schema' AND has_schema_privilege(current_user,n.oid,'CREATE')),
 'table_write', EXISTS(SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
   WHERE n.nspname='public' AND c.relkind IN ('r','p','v','m','f')
   AND has_table_privilege(current_user,c.oid,'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER')),
 'sequence_rights', EXISTS(SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
   WHERE n.nspname='public' AND c.relkind='S' AND has_sequence_privilege(current_user,c.oid,'USAGE,SELECT,UPDATE')),
 'owned_objects', EXISTS(SELECT 1 FROM pg_class WHERE relowner=r.oid)
   OR EXISTS(SELECT 1 FROM pg_namespace WHERE nspowner=r.oid)
   OR EXISTS(SELECT 1 FROM pg_proc WHERE proowner=r.oid)
   OR EXISTS(SELECT 1 FROM pg_database WHERE datdba=r.oid),
 'role_memberships', EXISTS(SELECT 1 FROM pg_auth_members WHERE member=r.oid),
 'public_function_execute', EXISTS(SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
   WHERE n.nspname='public' AND has_function_privilege(current_user,p.oid,'EXECUTE')))
FROM pg_roles r WHERE r.rolname=current_user;
