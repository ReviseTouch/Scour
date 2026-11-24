//! How words are read.

use super::*;

pub(crate) fn wire(wiring: &Wiring) {
    let Wiring {
        window,
        cat,
        state,
        link,
        ..
    } = *wiring;
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        window.on_field_words_picked(move |words| {
            if let Some(w) = weak.upgrade() {
                w.set_field_words(words);
                // Drawn again, the headers as now chosen.
                w.invoke_query_changed(w.get_query());
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    field_words: Some(words),
                    ..Default::default()
                },
            });
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        window.on_typos_strict_picked(move |strict| {
            if let Some(w) = weak.upgrade() {
                w.set_typos_strict(strict);
                // Asked again once the service has the new rule.
                let weak = w.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_millis(150), move || {
                    if let Some(w) = weak.upgrade() {
                        w.invoke_query_changed(w.get_query());
                    }
                });
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    typos_strict: Some(strict),
                    ..Default::default()
                },
            });
        });
    }
    // Kept by the service, which reads every query: the answer in sight changes
    // when the service says its index would answer differently.
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        window.on_accents_picked(move |ignored| {
            if let Some(w) = weak.upgrade() {
                w.set_accents_ignored(ignored);
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    accents_exact: Some(!ignored),
                    ..Default::default()
                },
            });
        });
    }
    {
        let weak = window.as_weak();
        let link = Rc::clone(link);
        let cat = Rc::clone(cat);
        window.on_typos_picked(move |choice| {
            if let Some(w) = weak.upgrade() {
                w.set_typos_choice(choice.clone());
                w.set_typos_said(typos_said(&cat.borrow(), &choice).into());
                // Asked again once the service has the new rule.
                let weak = w.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_millis(150), move || {
                    if let Some(w) = weak.upgrade() {
                        w.invoke_query_changed(w.get_query());
                    }
                });
            }
            link.send(Ask::Remember {
                change: scour_settings::Change {
                    typos: Some(choice.to_string()),
                    ..Default::default()
                },
            });
        });
    }
    {
        let weak = window.as_weak();
        let state = Rc::clone(state);
        window.on_similar_clicked(move || {
            let Some(w) = weak.upgrade() else { return };
            let text = state.borrow().similar.clone();
            if text.is_empty() {
                return;
            }
            w.set_query(text.as_str().into());
            w.invoke_query_changed(text.into());
            w.invoke_caret_to_end();
        });
    }
}
