//! Boot/integration test: runs every schema bootstrap and a representative
//! data flow against a real Postgres, proving the DDL + queries are valid.
//! Skips unless TEST_DATABASE_URL is set.
//!
//! Run: TEST_DATABASE_URL=postgres://postgres:changeme@127.0.0.1:5432/broodlink_hot \
//!        cargo test -p workspace-api boots_against_postgres -- --nocapture

#[cfg(test)]
mod t {
    use sqlx::postgres::PgPoolOptions;

    #[tokio::test]
    async fn boots_against_postgres() {
        let url = std::env::var("TEST_DATABASE_URL").unwrap_or_default();
        if url.is_empty() {
            eprintln!("skip: TEST_DATABASE_URL unset");
            return;
        }
        let pg = PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect to postgres");
        println!("connected to postgres");

        // Optional dev migration: drop a legacy table so it recreates with the
        // renamed column. Guarded so normal runs never destroy data.
        if std::env::var("BOOT_RESET").is_ok() {
            let _ = sqlx::raw_sql("DROP TABLE IF EXISTS ws_scheduled_emails CASCADE")
                .execute(&pg)
                .await;
            println!("BOOT_RESET: dropped legacy ws_scheduled_emails");
        }

        // 1. Every schema bootstrap (the never-tested DDL).
        crate::ensure_schema(&pg).await.expect("notes/tasks schema");
        crate::auth::ensure_auth_schema(&pg)
            .await
            .expect("auth schema");
        crate::calendar::ensure_calendar_schema(&pg)
            .await
            .expect("calendar schema");
        crate::email::ensure_email_schema(&pg)
            .await
            .expect("email schema");
        crate::chat::ensure_chat_schema(&pg)
            .await
            .expect("chat schema");
        crate::memory::ensure_memory_schema(&pg)
            .await
            .expect("memory schema");
        crate::documents::ensure_documents_schema(&pg)
            .await
            .expect("documents schema");
        crate::presets::ensure_presets_schema(&pg)
            .await
            .expect("presets schema");
        crate::skills::ensure_skills_schema(&pg)
            .await
            .expect("skills schema");
        crate::compare::ensure_compare_schema(&pg)
            .await
            .expect("compare schema");
        crate::signatures::ensure_signatures_schema(&pg)
            .await
            .expect("signatures schema");
        crate::contacts::ensure_contacts_schema(&pg)
            .await
            .expect("contacts schema");
        crate::gallery::ensure_gallery_schema(&pg)
            .await
            .expect("gallery schema");
        crate::webhooks::ensure_webhooks_schema(&pg)
            .await
            .expect("webhooks schema");
        crate::vault::ensure_vault_schema(&pg)
            .await
            .expect("vault schema");
        crate::research::ensure_research_schema(&pg)
            .await
            .expect("research schema");
        crate::embeddings::ensure_embeddings_schema(&pg)
            .await
            .expect("embeddings schema");
        println!("all schemas created");

        // 2. Representative CRUD across every subsystem, incl. foreign keys.
        let o = "boottest";
        sqlx::query(
            "INSERT INTO dashboard_users (id, username, password_hash, role) \
             VALUES ('boot-u1', 'boottest', 'x', 'admin') ON CONFLICT (username) DO NOTHING",
        )
        .execute(&pg)
        .await
        .expect("dashboard_users insert");

        sqlx::query("INSERT INTO ws_notes (id, owner, title) VALUES ('boot-n1', $1, 'hi')")
            .bind(o)
            .execute(&pg)
            .await
            .expect("note insert");

        sqlx::query(
            "INSERT INTO ws_scheduled_tasks (id, owner, name) VALUES ('boot-t1', $1, 'job')",
        )
        .bind(o)
        .execute(&pg)
        .await
        .expect("task insert");

        sqlx::query("INSERT INTO ws_sessions (id, owner, name) VALUES ('boot-s1', $1, 's')")
            .bind(o)
            .execute(&pg)
            .await
            .expect("session insert");
        sqlx::query(
            "INSERT INTO ws_chat_messages (id, session_id, role, content) \
             VALUES ('boot-m1', 'boot-s1', 'user', 'hello')",
        )
        .execute(&pg)
        .await
        .expect("chat message insert (FK -> ws_sessions)");

        sqlx::query("INSERT INTO ws_memories (id, owner, text) VALUES ('boot-mem1', $1, 'a fact')")
            .bind(o)
            .execute(&pg)
            .await
            .expect("memory insert");

        sqlx::query(
            "INSERT INTO ws_calendars (id, owner, name) VALUES ('boot-c1', $1, 'Personal')",
        )
        .bind(o)
        .execute(&pg)
        .await
        .expect("calendar insert");
        sqlx::query(
            "INSERT INTO ws_calendar_events (uid, calendar_id, dtstart, dtend) \
             VALUES ('boot-e1', 'boot-c1', now()::timestamp, now()::timestamp)",
        )
        .execute(&pg)
        .await
        .expect("event insert (FK -> ws_calendars)");

        sqlx::query(
            "INSERT INTO ws_email_accounts (id, owner, name) VALUES ('boot-ea1', $1, 'acct')",
        )
        .bind(o)
        .execute(&pg)
        .await
        .expect("email account insert");

        sqlx::query("INSERT INTO ws_documents (id, owner, title, current_content) VALUES ('boot-d1', $1, 'Doc', 'hi')")
            .bind(o)
            .execute(&pg)
            .await
            .expect("document insert");
        sqlx::query(
            "INSERT INTO ws_document_versions (id, document_id, version_number, content) \
             VALUES ('boot-dv1', 'boot-d1', 1, 'hi')",
        )
        .execute(&pg)
        .await
        .expect("document version insert (FK -> ws_documents)");
        sqlx::query("INSERT INTO ws_editor_drafts (id, owner, name, payload) VALUES ('boot-ed1', $1, 'd', '{}')")
            .bind(o)
            .execute(&pg)
            .await
            .expect("editor draft insert");
        sqlx::query(
            "INSERT INTO ws_presets (owner, pid, kind, data) VALUES ($1, 'custom', 'custom', '{}')",
        )
        .bind(o)
        .execute(&pg)
        .await
        .expect("preset insert");
        sqlx::query(
            "INSERT INTO ws_skills (id, owner, name, data) VALUES ('boot-sk1', $1, 'Skill', '{}')",
        )
        .bind(o)
        .execute(&pg)
        .await
        .expect("skill insert");
        sqlx::query("INSERT INTO ws_comparisons (id, owner, prompt, model_a, model_b) VALUES ('boot-cmp1', $1, 'p', 'm-a', 'm-b')")
            .bind(o)
            .execute(&pg)
            .await
            .expect("comparison insert");
        sqlx::query("INSERT INTO ws_signatures (id, owner, name, data_png) VALUES ('boot-sig1', $1, 'Sig', 'eHg=')")
            .bind(o)
            .execute(&pg)
            .await
            .expect("signature insert");
        sqlx::query("INSERT INTO ws_contacts (owner, uid, name, emails) VALUES ($1, 'boot-ct1', 'Jane', '[\"j@x.com\"]')")
            .bind(o)
            .execute(&pg)
            .await
            .expect("contact insert");
        sqlx::query(
            "INSERT INTO ws_gallery_albums (id, owner, name) VALUES ('boot-al1', $1, 'Album')",
        )
        .bind(o)
        .execute(&pg)
        .await
        .expect("album insert");
        sqlx::query("INSERT INTO ws_gallery_images (id, owner, filename, album_id) VALUES ('boot-img1', $1, 'boot-img1.png', 'boot-al1')")
            .bind(o)
            .execute(&pg)
            .await
            .expect("gallery image insert");
        sqlx::query("INSERT INTO ws_webhooks (id, owner, name, url, events) VALUES ('boot-wh1', $1, 'wh', 'https://x', 'chat.completed')")
            .bind(o)
            .execute(&pg)
            .await
            .expect("webhook insert");
        sqlx::query("INSERT INTO ws_vault_config (owner, server_url) VALUES ($1, 'https://v')")
            .bind(o)
            .execute(&pg)
            .await
            .expect("vault config insert");
        sqlx::query(
            "INSERT INTO ws_research (session_id, owner, query) VALUES ('boot-rp1', $1, 'q')",
        )
        .bind(o)
        .execute(&pg)
        .await
        .expect("research insert");
        sqlx::query("INSERT INTO ws_embedding_config (owner, url) VALUES ($1, 'https://e')")
            .bind(o)
            .execute(&pg)
            .await
            .expect("embedding config insert");

        // Read back a couple to prove queries work.
        let notes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ws_notes WHERE owner = $1")
            .bind(o)
            .fetch_one(&pg)
            .await
            .unwrap();
        let msgs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM ws_chat_messages WHERE session_id = 'boot-s1'",
        )
        .fetch_one(&pg)
        .await
        .unwrap();
        assert!(notes >= 1 && msgs >= 1);

        // 3. Cleanup (cascades handle child rows).
        for table in [
            "ws_sessions",
            "ws_notes",
            "ws_scheduled_tasks",
            "ws_memories",
            "ws_calendars",
            "ws_email_accounts",
            "ws_documents",
            "ws_editor_drafts",
            "ws_presets",
            "ws_skills",
            "ws_comparisons",
            "ws_signatures",
            "ws_contacts",
            "ws_gallery_albums",
            "ws_gallery_images",
            "ws_webhooks",
            "ws_vault_config",
            "ws_research",
            "ws_embedding_config",
        ] {
            let _ = sqlx::query(&format!("DELETE FROM {table} WHERE owner = $1"))
                .bind(o)
                .execute(&pg)
                .await;
        }
        let _ = sqlx::query("DELETE FROM dashboard_users WHERE username = 'boottest'")
            .execute(&pg)
            .await;

        println!(
            "BOOT OK: 6 schema bootstraps + CRUD across notes, tasks, sessions, \
             chat messages (FK), memory, calendar+events (FK), email accounts"
        );
    }
}
