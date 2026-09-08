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
