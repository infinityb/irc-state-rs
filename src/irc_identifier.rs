use irc::irccase::IrcAsciiExt;

fn channel_deprefix(target: &str) -> &str {
    match target.find('#') {
        Some(idx) => &target[idx..],
        None => target
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct IrcIdentifier(String);

impl IrcIdentifier {
    pub fn from_str(mut val: &str) -> IrcIdentifier {
        val = channel_deprefix(val);
        IrcIdentifier(val.to_irc_lower())
    }

    pub fn as_slice(&self) -> &str {
        let IrcIdentifier(ref string) = *self;
        &string[..]
    }
}