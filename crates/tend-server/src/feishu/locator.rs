use super::source_position;
use serde_json::Value;
use url::Url;

pub(super) struct Link {
    url: Url,
    chat: Option<String>,
    topic: Option<String>,
    chat_position: Option<u64>,
    topic_position: Option<u64>,
}

impl Link {
    pub(super) fn parse(raw: &str) -> Option<Self> {
        fn report<T: Eq>(field: &mut Option<T>, value: T) -> Option<()> {
            if field.as_ref().is_some_and(|old| *old != value) {
                return None;
            }
            *field = Some(value);
            Some(())
        }

        let url = Url::parse(raw).ok()?;
        if url.scheme() != "https"
            || url.host_str() != Some("applink.feishu.cn")
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || !url.path().starts_with("/client/")
        {
            return None;
        }
        let mut link = Self {
            url,
            chat: None,
            topic: None,
            chat_position: None,
            topic_position: None,
        };
        for (key, value) in link
            .url
            .query_pairs()
            .filter(|(_, value)| !value.is_empty())
        {
            match key.as_ref() {
                "openChatId" | "openchatid" | "open_chat_id" => {
                    report(&mut link.chat, value.into_owned())?;
                }
                "openthreadid" | "open_thread_id" => {
                    report(&mut link.topic, value.into_owned())?;
                }
                "position" => report(&mut link.chat_position, value.parse().ok()?)?,
                "thread_position" => report(&mut link.topic_position, value.parse().ok()?)?,
                _ => (),
            }
        }
        Some(link)
    }

    pub(super) fn agrees(&self, chat: &str, topic: Option<&str>, position: Option<u64>) -> bool {
        let link_position = if topic.is_some() {
            self.topic_position
        } else {
            self.chat_position
        };
        self.chat.as_deref().is_none_or(|id| id == chat)
            && self.topic.as_deref().is_none_or(|id| Some(id) == topic)
            && link_position.zip(position).is_none_or(|(a, b)| a == b)
    }

    fn topic_position(&self, topic: &str) -> Option<u64> {
        (self.url.path() == "/client/thread/open" && self.topic.as_deref() == Some(topic))
            .then_some(self.topic_position)
            .flatten()
    }

    fn chat_position(&self, chat: &str) -> Option<u64> {
        (self.url.path() == "/client/chat/open" && self.chat.as_deref() == Some(chat))
            .then_some(self.chat_position)
            .flatten()
    }
}

pub(crate) fn topic_position(href: &str, topic: &str) -> Option<u64> {
    Link::parse(href)?.topic_position(topic)
}

pub(super) fn resolve(raw: &Value, chat: &str, topic: Option<&str>) -> (Option<u64>, String, bool) {
    // A position in the chat is not a position inside a topic. Never combine
    // a provider URL with a conflicting position or a different container.
    let field = if topic.is_some() {
        "thread_message_position"
    } else {
        "message_position"
    };
    let native = source_position(&raw[field]);
    let position = native.or_else(|| {
        let link = Link::parse(raw["message_app_link"].as_str()?)?;
        if !link.agrees(chat, topic, native) {
            return None;
        }
        match topic {
            Some(id) => link.topic_position(id),
            None => link.chat_position(chat),
        }
    });
    let href = match (topic, position) {
        (Some(id), Some(pos)) => format!(
            "https://applink.feishu.cn/client/thread/open?openthreadid={id}&openchatid={chat}&open_thread_id={id}&open_chat_id={chat}&thread_position={pos}"
        ),
        (None, Some(pos)) => {
            format!("https://applink.feishu.cn/client/chat/open?openChatId={chat}&position={pos}")
        }
        (_, None) => format!("https://applink.feishu.cn/client/chat/open?openChatId={chat}"),
    };
    (position, href, position.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_coordinates_win_over_conflicting_links() {
        for (chat, topic, pos) in [
            ("oc_a", "omt_a", 999),
            ("oc_other", "omt_a", 9),
            ("oc_a", "omt_other", 9),
        ] {
            let raw = json!({
                "thread_message_position":"9",
                "message_app_link":format!("https://applink.feishu.cn/client/thread/open?openthreadid={topic}&openchatid={chat}&thread_position={pos}")
            });
            let (position, href, chat_only) = resolve(&raw, "oc_a", Some("omt_a"));
            assert_eq!(position, Some(9));
            assert_eq!(topic_position(&href, "omt_a"), Some(9));
            assert!(!chat_only);
        }
    }

    #[test]
    fn a_sparse_row_can_use_only_a_consistent_scoped_link() {
        let base = "https://applink.feishu.cn/client/thread/open?openthreadid=omt_a&openchatid=oc_a&thread_position=9";
        let raw = json!({"message_position":"356","message_app_link":base});
        assert_eq!(resolve(&raw, "oc_a", Some("omt_a")).0, Some(9));
        for conflict in [
            "open_thread_id=omt_other",
            "open_chat_id=oc_other",
            "thread_position=999",
        ] {
            let raw = json!({"message_app_link":format!("{base}&{conflict}")});
            let (position, href, chat_only) = resolve(&raw, "oc_a", Some("omt_a"));
            assert_eq!(position, None);
            assert_eq!(
                href,
                "https://applink.feishu.cn/client/chat/open?openChatId=oc_a"
            );
            assert!(chat_only);
        }
    }
}
