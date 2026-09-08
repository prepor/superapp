use super::*;
use kernel::filter;
use kernel::richtable::Datasource;

#[test]
fn chat_filters_ignore_unicode_case_in_titles_previews_and_folders() {
    let mut s = session();
    s.store()
        .write(|c| {
            c.execute(
                "UPDATE tg_peer SET name = 'Рабочий Чат' WHERE id = ?1",
                [STELAXIS],
            )?;
            c.execute(
                "UPDATE tg_folder SET name = 'Работа' WHERE name = 'work'",
                [],
            )?;
            c.execute(
                "UPDATE tg_message SET text = 'ПРИВЕТ ЁЖИК' WHERE seq =
             (SELECT seq FROM tg_message WHERE chat = ?1 ORDER BY date DESC, id DESC LIMIT 1)",
                [HIKE],
            )?;
            Ok(())
        })
        .unwrap();
    let list = open_root(&mut s, Chats::id());
    for (filter, title) in [
        ("рАбОчИй", "Рабочий Чат"),
        ("РАБОЧИЙ ЧАТ", "Рабочий Чат"),
        ("привет ёж", "Hiking Saturday"),
        ("@folder:рАбОтА @kind:GROUP", "Рабочий Чат"),
    ] {
        with_chats(&s, list, |c| {
            c.list_mut().set_filter(filter);
        });
        assert_eq!(titles(&s, list), vec![title], "{filter}");
    }

    // Missing folders still belong to the complement of a text tag.
    with_chats(&s, list, |c| {
        c.list_mut().set_filter("@not:folder:РАБОТА");
    });
    let remaining = titles(&s, list);
    assert!(remaining.iter().any(|title| title == "Andrey Rudenko"));
    assert!(!remaining.iter().any(|title| title == "Рабочий Чат"));
}

#[test]
fn message_and_people_filters_ignore_unicode_case() {
    let s = session();
    s.store()
        .write(|c| {
            c.execute(
                "UPDATE tg_peer SET name = 'Рабочий Чат' WHERE id = ?1",
                [STELAXIS],
            )?;
            c.execute(
                "UPDATE tg_peer SET name = 'ИВАН Петров' WHERE id = ?1",
                [IVAN],
            )?;
            c.execute(
                "INSERT INTO tg_message(id, chat, sender, date, text)
             VALUES(900001, ?1, ?2, 9999999999, 'ПрИвЕт, ЁЖИК!')",
                [STELAXIS, IVAN],
            )?;
            Ok(())
        })
        .unwrap();
    for query in ["пРиВеТ", "@from:иВаН @chat:\"рАбОчИй чАт\" ёжик"] {
        let ast = filter::parse(query).ast;
        assert_eq!(
            model::MESSAGES.count(s.store(), ast.as_ref()),
            Some(1),
            "{query}"
        );
        let hits = model::MESSAGES.page(s.store(), ast.as_ref(), 0, 50);
        assert_eq!(hits.len(), 1, "{query}");
        assert_eq!(hits[0].id, 900001);
    }
    for (source, query) in [
        (&model::CONTACTS, "иВаН"),
        (&model::MEMBERS, "@group:\"РАБОЧИЙ ЧАТ\" иван"),
    ] {
        let ast = filter::parse(query).ast;
        let people = source.page(s.store(), ast.as_ref(), 0, 50);
        assert_eq!(people.len(), 1, "{query}");
        assert_eq!(people[0].id, IVAN);
    }
}

#[test]
fn search_panel_finds_unicode_names_and_messages() {
    const CASES: &[(&str, &str)] = &[
        ("Привет", "пРиВеТ"),
        ("ÉCOLE", "école"),
        ("ΟΔΟΣ", "οδος"),
        ("Straße", "STRASSE"),
        (r"Путь 100%_\Файл", r"пУТЬ 100%_\фАЙЛ"),
    ];
    let s = session();
    s.store()
        .write(|c| {
            c.execute("UPDATE tg_peer SET name = 'Привет' WHERE id = ?1", [VERA])?;
            for (i, (text, _)) in CASES.iter().enumerate() {
                c.execute(
                    "INSERT INTO tg_message(id, chat, sender, date, text)
                 VALUES(?1, ?2, ?2, 9999999999, ?3)",
                    rusqlite::params![900001 + i as i64, VERA, text],
                )?;
            }
            c.execute(
                "INSERT INTO tg_message(id, chat, sender, date, text, service)
             VALUES(900010, ?1, ?1, 9999999999, 'Привет', 1),
                   (900011, ?1, ?1, 9999999999, 'Путь 100XY\\Файл', 0)",
                [VERA],
            )?;
            Ok(())
        })
        .unwrap();
    let mut engine = Engine::inline(s.apps().providers());
    for (i, (text, query)) in CASES.iter().enumerate() {
        engine.ask(s.store(), i as u64 + 1, query);
        let hits: Vec<_> = engine
            .collect()
            .into_iter()
            .flat_map(|a| a.hits)
            .map(|hit| (hit.label, hit.go))
            .collect();
        let mut expected = Vec::new();
        if i == 0 {
            expected.push((text.to_string(), Go::Open(Chat::id(VERA))));
        }
        expected.push((
            text.to_string(),
            Go::Open(Chat::at(VERA, 900001 + i as i64)),
        ));
        assert_eq!(hits, expected, "{query}");
    }
}

#[test]
fn indexed_messages_preserve_literal_substrings_and_refresh_after_writes() {
    let s = session();
    s.store().write(|c| {
        c.execute("DELETE FROM tg_message", [])?;
        for (id, text) in [
            (1, "a thermos in Straße · ПРИВЕТ ЁЖИК · ΟΔΟΣ · ÉCOLE"),
            (2, "abc bcd aaa"),
            (3, "abcd aaaa"),
            (4, r#"100%_\ "quoted" AND 🦩🦩"#),
            (5, r#"100XY\ quoted AND 🦩"#),
            (6, "one two"),
            (7, "two one"),
        ] {
            c.execute("INSERT INTO tg_message(id, chat, sender, date, text)
                VALUES(?1, ?2, ?2, ?1, ?3)", rusqlite::params![id, VERA, text])?;
        }
        c.execute("INSERT INTO tg_message(id, chat, sender, date, text, service)
            VALUES(1, ?1, ?1, 10, 'thermos', 1)", [HIKE])?;
        Ok(())
    }).unwrap();

    for query in ["a", "rm", "erm", "hermos", "ß", "SS", "STRASSE", "ё", "ёт",
        "рив", "привет ёж", "οδος", "école", "abcd", "aaaa", r"100%_\",
        "\"quoted\"", "AND", "🦩", "🦩🦩", "one two", "two one", "absent"]
    {
        let ast = kernel::filter::Ast::Text(query.into());
        let expected: Vec<i64> = s.store().conn().prepare(
            "SELECT seq FROM tg_message WHERE service = 0
             AND casefold(text) LIKE casefold(?) ESCAPE '\\' ORDER BY date DESC, seq DESC",
        ).unwrap().query_map([format!("%{}%", query.replace('\\', "\\\\")
            .replace('%', "\\%").replace('_', "\\_"))], |r| r.get(0))
            .unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(model::MESSAGES.count(s.store(), Some(&ast)), Some(expected.len()), "{query}");
        assert_eq!(model::MESSAGES.page(s.store(), Some(&ast), 0, 50)
            .iter().map(|m| m.seq).collect::<Vec<_>>(), expected, "{query}");
    }

    let ast = kernel::filter::Ast::Text("hermos".into());
    assert_eq!(model::MESSAGES.count(s.store(), Some(&ast)), Some(1));
    s.store().write(|c| {
        c.execute("UPDATE tg_message SET text = 'replaced' WHERE chat = ?1 AND id = 1", [VERA])?;
        c.execute("UPDATE tg_message SET service = 0 WHERE chat = ?1 AND id = 1", [HIKE])?;
        Ok(())
    }).unwrap();
    let hits = model::MESSAGES.page(s.store(), Some(&ast), 0, 50);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].chat, HIKE);
    s.store().write(|c| c.execute("DELETE FROM tg_message WHERE chat = ?1", [HIKE]).map(|_| ())).unwrap();
    assert_eq!(model::MESSAGES.count(s.store(), Some(&ast)), Some(0));
}

#[test]
fn selective_message_search_does_not_walk_the_history() {
    use rusqlite::StatementStatus;
    let s = session();
    s.store().write(|c| {
        c.execute("DELETE FROM tg_message", [])?;
        c.execute("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i + 1 FROM n WHERE i < 11000)
            INSERT INTO tg_message(id, chat, date, text)
            SELECT 900000 + i, ?1, i, 'ordinary cached message' FROM n", [VERA]).map(|_| ())
    }).unwrap();
    for query in ["🦩", "🦩🦩", "🦩🦩🦩", "absent search text"] {
        let ast = kernel::filter::Ast::Text(query.into());
        for q in [model::MESSAGES.sql.spec.count(model::MESSAGES.sql.tags, Some(&ast)),
            model::MESSAGES.sql.spec.page(model::MESSAGES.sql.tags, Some(&ast), 0, 50)]
        {
            let mut stmt = s.store().conn().prepare(&q.sql).unwrap();
            let mut rows = stmt.query(rusqlite::params_from_iter(&q.params)).unwrap();
            while rows.next().unwrap().is_some() {}
            drop(rows);
            assert!(stmt.get_status(StatementStatus::VmStep) < 1000,
                "{query:?} walked the history: {} steps", stmt.get_status(StatementStatus::VmStep));
        }
    }
    // A broad posting list uses the date index for pages. Scoping the same
    // query to a chat must preserve both the total and stable page boundaries.
    for query in ["ord", "@chat:vera ord"] {
        let ast = filter::parse(query).ast;
        assert_eq!(model::MESSAGES.count(s.store(), ast.as_ref()), Some(11000));
        let page = model::MESSAGES.page(s.store(), ast.as_ref(), 0, 3);
        assert_eq!(page.iter().map(|m| m.id).collect::<Vec<_>>(), [911000, 910999, 910998]);
        let next = model::MESSAGES.page(s.store(), ast.as_ref(), 3, 3);
        assert_eq!(next.iter().map(|m| m.id).collect::<Vec<_>>(), [910997, 910996, 910995]);
        assert_eq!(model::MESSAGES.index_of(s.store(), ast.as_ref(), &next[0]), Some(3));
    }
}
