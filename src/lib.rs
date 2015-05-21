#![feature(into_cow, convert)]
#![warn(dead_code)]
#![deny(unused_variables, unused_mut)]

#[macro_use] extern crate log;
extern crate irc;

mod irc_identifier;


use std::default::Default;
use std::collections::{
    hash_map,
    HashMap,
    HashSet,
};
use std::borrow::IntoCow;
use std::ops::Deref;

use irc::message_types::server as irc_server;
use irc::parse::{IrcMsg, IrcMsgPrefix};
use irc::{
    JoinSuccess,
    WhoRecord,
    WhoSuccess,
    IrcEvent
};

use irc_identifier::IrcIdentifier;

pub use MessageEndpoint::{
    KnownUser,
    KnownChannel,
    AnonymousUser,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MessageEndpoint {
    KnownUser(UserId),
    KnownChannel(ChannelId),
    Server(String),
    AnonymousUser,
}

#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct UserId(u64);


#[derive(Clone, PartialEq, Eq, Debug)]
pub struct User {
    id: UserId,
    prefix: IrcMsgPrefix<'static>,
    channels: HashSet<ChannelId>
}

impl User {
    fn from_who(id: UserId, who: &WhoRecord) -> User {
        User {
            id: id,
            prefix: who.get_prefix().to_owned(),
            channels: Default::default(),
        }
    }

    pub fn get_nick(&self) -> &str {
        let prefix = self.prefix.as_slice();
        match prefix.find('!') {
            Some(idx) => &prefix[0..idx],
            None => prefix
        }
    }

    fn set_nick(&mut self, nick: &str) {
        self.prefix = self.prefix.with_nick(nick).expect("Need nicked prefix");
    }
}


#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChannelId(u64);


#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Channel {
    id: ChannelId,
    name: String,
    topic: String,
    users: HashSet<UserId>
}

impl Channel {
    fn from_info(chan_info: &ChannelInfo) -> Channel {
        Channel {
            id: chan_info.id,
            name: chan_info.name.clone(),
            topic: chan_info.topic.clone(),
            users: Default::default(),
        }
    }

    fn set_topic(&mut self, topic: &str) {
        self.topic.clear();
        self.topic.push_str(topic);
    }
}

#[derive(Debug)]
struct ChannelInfo {
    id: ChannelId,
    name: String,
    topic: String
}

impl ChannelInfo {
    fn from_join(id: ChannelId, join: &JoinSuccess) -> ChannelInfo {
        let topic = String::from_utf8(match join.topic {
            Some(ref topic) => topic.text.clone(),
            None => Vec::new()
        }).ok().expect("non-string");

        let channel_name = ::std::str::from_utf8(join.channel.as_slice())
            .ok().expect("bad chan").to_string();

        ChannelInfo {
            id: id,
            name: channel_name,
            topic: topic
        }
    }
}

pub struct FrozenState(State);

impl Deref for FrozenState {
    type Target = State;

    fn deref<'a>(&'a self) -> &'a State {
        let FrozenState(ref state) = *self;
        state
    }
}

unsafe impl Send for FrozenState {}
unsafe impl Sync for FrozenState {}

#[derive(Debug, Clone)]
pub struct State {
    // Can this be made diffable by using sorted `users`, `channels`,
    // `users[].channels` and `channels[].users`?  TreeSet.
    user_seq: u64,
    channel_seq: u64,

    self_nick: String,
    self_id: UserId,

    user_map: HashMap<IrcIdentifier, UserId>,
    users: HashMap<UserId, User>,

    channel_map: HashMap<IrcIdentifier, ChannelId>,
    channels: HashMap<ChannelId, Channel>,

    generation: u64,
}

impl State {
    pub fn new() -> State {
        State {
            user_seq: 1,
            channel_seq: 0,
            self_nick: String::new(),
            user_map: Default::default(),
            users: Default::default(),
            self_id: UserId(0),
            channel_map: Default::default(),
            channels: Default::default(),
            generation: 0,
        }
    }

    fn on_other_part(&mut self, part: &irc_server::Part) {
        let channel_name = IrcIdentifier::from_str(part.get_channel());
        let user_nick = IrcIdentifier::from_str(part.get_nick());

        let opt_chan_id = self.channel_map.get(&channel_name).and_then(|&v| Some(v));
        if opt_chan_id.is_none() {
            warn!("Got channel {:?} without knowing about it.", part.get_channel());
        }

        let opt_user_id = self.user_map.get(&user_nick).and_then(|&v| Some(v));
        if opt_user_id.is_none() {
            warn!("Got user {:?} without knowing about it.", part.get_nick());
        }

        let (chan_id, user_id) = match (opt_chan_id, opt_user_id) {
            (Some(chan_id), Some(user_id)) => (chan_id, user_id),
            _ => return,
        };

        self.validate_state_internal_panic();
        self.unlink_user_channel(user_id, chan_id);
        self.validate_state_internal_panic();
    }

    fn on_self_part(&mut self, part: &irc_server::Part) {
        assert!(self.remove_channel_by_name(part.get_channel()).is_some());
    }

    fn on_other_quit(&mut self, quit: &irc_server::Quit) {
        assert!(self.remove_user_by_nick(quit.get_nick()).is_some());
    }

    fn on_other_join(&mut self, join: &irc_server::Join) {
        let channel_name = IrcIdentifier::from_str(join.get_channel());
        let user_nick = IrcIdentifier::from_str(join.get_nick());

        let chan_id = match self.channel_map.get(&channel_name) {
            Some(chan_id) => *chan_id,
            None => panic!("Got message for channel {:?} without knowing about it.", channel_name)
        };

        let (is_create, user_id) = match self.user_map.get(&user_nick) {
            Some(user_id) => {
                (false, *user_id)
            },
            None => {
                let new_user_id = UserId(self.user_seq);
                self.user_seq += 1;
                (true, new_user_id)
            }
        };
        if is_create {
            let user = User {
                id: user_id,
                prefix: join.to_irc_msg().get_prefix().to_owned(),
                channels: HashSet::new(),
            };
            self.users.insert(user_id, user);
            self.user_map.insert(user_nick, user_id);
        }
        self.users.get_mut(&user_id).expect("user not found").channels.insert(chan_id);

        assert!(self.update_channel_by_name(channel_name.as_slice(), |channel| {
            channel.users.insert(user_id);
        }), "Got message for channel {:?} without knowing about it.");
    }

    fn on_self_join(&mut self, join: &JoinSuccess) {
        let channel_name = ::std::str::from_utf8(join.channel.as_slice()).ok().unwrap();
        let channel_name = IrcIdentifier::from_str(channel_name);

        if let Some(_) = self.channel_map.get(&channel_name) {
            warn!("Joining already joined channel {:?}; skipped", join.channel);
            return;
        }
        warn!("users = {:?}", join.nicks);
        let new_chan_id = ChannelId(self.channel_seq);
        self.channel_seq += 1;

        self.channels.insert(new_chan_id, Channel::from_info(
            &ChannelInfo::from_join(new_chan_id, join)));
        self.channel_map.insert(channel_name.clone(), new_chan_id);
    }

    fn validate_state_with_who(&self, who: &WhoSuccess) {
        let channel_name = ::std::str::from_utf8(who.channel.as_slice()).ok().unwrap();
        let channel_name = IrcIdentifier::from_str(channel_name);

        let (_, channel) = match self.get_channel_by_name(channel_name.as_slice()) {
            Some(chan_pair) => chan_pair,
            None => return
        };

        info!("Validating channel state");
        let mut known_users = HashSet::new();
        for user in channel.users.iter() {
            match self.users.get(user) {
                Some(user) => {
                    known_users.insert(user.get_nick().to_string());
                },
                None => panic!("Inconsistent state"),
            }
        }

        let mut valid_users = HashSet::new();
        for rec in who.who_records.iter() {
            valid_users.insert(rec.nick.clone());
        }

        let mut is_valid = true;
        for valid_unknowns in valid_users.difference(&known_users) {
            warn!("Valid but unknown nick: {:?}", valid_unknowns);
            is_valid = false;
        }

        for invalid_knowns in known_users.difference(&valid_users) {
            warn!("Known but invalid nick: {:?}", invalid_knowns);
            is_valid = false;
        }

        if is_valid {
            info!("Channel state has been validated: sychronized");
        } else {
            warn!("Channel state has been validated: desynchronized!");
        }
    }

    fn on_who(&mut self, who: &WhoSuccess) {
        // If we WHO a channel that we aren't in, we aren't changing any
        // state.
        let channel_name = ::std::str::from_utf8(who.channel.as_slice()).ok().unwrap();
        let channel_name = IrcIdentifier::from_str(channel_name);

        let chan_id = match self.get_channel_by_name(channel_name.as_slice()) {
            Some((chan_id, channel)) => {
                if !channel.users.is_empty() {
                    self.validate_state_with_who(who);
                    return;
                }
                chan_id
            }
            None => return
        };

        let mut users = Vec::with_capacity(who.who_records.len());
        let mut user_ids = Vec::with_capacity(who.who_records.len());

        for rec in who.who_records.iter() {
            let nick = IrcIdentifier::from_str(&rec.nick);
            user_ids.push(match self.user_map.get(&nick) {
                Some(user_id) => *user_id,
                None => {
                    let new_user_id = UserId(self.user_seq);
                    self.user_seq += 1;
                    users.push(User::from_who(new_user_id, rec));
                    new_user_id
                }
            });
        }
        for user in users.into_iter() {
            self.insert_user(user);
        }
        for user_id in user_ids.iter() {
            match self.users.get_mut(user_id) {
                Some(user_state) => {
                    user_state.channels.insert(chan_id);
                },
                None => {
                    if *user_id != self.self_id {
                        panic!("{:?}", user_id);
                    }
                }
            };
        }

        let tmp_chan_name = channel_name.clone();
        assert!(self.update_channel_by_name(channel_name.as_slice(), move |channel| {
            let added = user_ids.len() - channel.users.len();
            info!("Added {:?} users for channel {:?}", added, tmp_chan_name);
            channel.users.extend(user_ids.into_iter());
        }), "Got message for channel {:?} without knowing about it.");
    }

    fn on_topic(&mut self, topic: &irc_server::Topic) {
        assert!(self.update_channel_by_name(topic.get_channel(), |channel| {
            let topic = String::from_utf8_lossy(topic.get_body_raw()).into_owned();
            channel.set_topic(&topic);
        }));
    }

    fn on_nick(&mut self, nick: &irc_server::Nick) {
        assert!(self.update_user_by_nick(nick.get_nick(), |user| {
            user.set_nick(nick.get_new_nick());
        }))
    }

    //
    fn on_kick(&mut self, kick: &irc_server::Kick) {
        let channel_name = IrcIdentifier::from_str(kick.get_channel());
        let kicked_user_nick = IrcIdentifier::from_str(kick.get_kicked_nick());

        let (chan_id, user_id) = match (
            self.channel_map.get(&channel_name),
            self.user_map.get(&kicked_user_nick)
        ) {
            (Some(chan_id), Some(user_id)) => (*chan_id, *user_id),
            (None, Some(_)) => {
                warn!("Strange: unknown channel {:?}", channel_name);
                return;
            },
            (Some(_), None) => {
                warn!("Strange: unknown nick {:?}", kicked_user_nick);
                return;
            },
            (None, None) => {
                warn!("Strange: unknown chan {:?} and nick {:?}", channel_name, kicked_user_nick);
                return;
            }
        };
        self.unlink_user_channel(user_id, chan_id);
    }

    pub fn is_self_join(&self, msg: &IrcMsg) -> Option<irc_server::Join> {
        use irc::message_types::server::IncomingMsg::Join;

        let is_self = msg.get_prefix().nick().and_then(|nick| {
            Some(nick == self.self_nick)
        }).unwrap_or(false);

        if !is_self {
            return None;
        }
        match irc_server::IncomingMsg::from_msg(msg.clone()) {
            Join(join) => Some(join),
            _ => None,
        }
    }

    pub fn on_message(&mut self, msg: &IrcMsg) {
        use irc::message_types::server::IncomingMsg::{
            Part, Quit, Join, Topic, Kick, Nick};

        let ty_msg = irc_server::IncomingMsg::from_msg(msg.clone());
        let is_self = msg.get_prefix().nick().and_then(|nick| {
            Some(nick == self.self_nick)
        }).unwrap_or(false);

        match (&ty_msg, is_self) {
            (&Part(ref part), true) => return self.on_self_part(part),
            (&Part(ref part), false) => return self.on_other_part(part),
            (&Quit(ref quit), false) => return self.on_other_quit(quit),
            // is this JOIN right?
            (&Join(ref join), false) => return self.on_other_join(join),
            (&Topic(ref topic), _) => return self.on_topic(topic),
            (&Nick(ref nick), _) => return self.on_nick(nick),
            (&Kick(ref kick), _) => return self.on_kick(kick),
            (_, _) => ()
        }

        if msg.get_command() == "001" {
            let channel_name = ::std::str::from_utf8(&msg[0]).ok().unwrap();
            self.initialize_self_nick(channel_name);
        }
    }

    pub fn on_event(&mut self, event: &IrcEvent) {
        let () = match *event {
            IrcEvent::IrcMsg(ref message) => self.on_message(message),
            IrcEvent::JoinBundle(Ok(ref join_bun)) => self.on_self_join(join_bun),
            IrcEvent::JoinBundle(Err(_)) => (),
            IrcEvent::WhoBundle(Ok(ref who_bun)) => self.on_who(who_bun),
            IrcEvent::WhoBundle(Err(_)) => (),
            IrcEvent::Extension(_) => {
                unimplemented!();
            }
        };
    }

    pub fn get_self_nick<'a>(&'a self) -> &'a str {
        &self.self_nick
    }

    pub fn set_self_nick(&mut self, new_nick_str: &str) {
        let new_nick = IrcIdentifier::from_str(new_nick_str);
        let old_nick = IrcIdentifier::from_str(&self.self_nick);
        if self.self_nick != "" {
            let user_id = match self.user_map.remove(&old_nick) {
                Some(user_id) => user_id,
                None => panic!("inconsistent user_map: {:?}[{:?}]",
                    self.user_map, self.self_nick)
            };
            self.user_map.insert(new_nick, user_id);
        }
        self.self_nick = new_nick_str.to_string();
    }

    fn initialize_self_nick(&mut self, new_nick_str: &str) {
        let new_nick = IrcIdentifier::from_str(new_nick_str);
        self.user_map.insert(new_nick, self.self_id);
        self.users.insert(self.self_id, User {
            id: self.self_id,
            // FIXME: hack
            prefix: IrcMsgPrefix::new(format!("{}!someone@somewhere", new_nick_str).into_cow()),
            channels: HashSet::new(),
        });
        self.set_self_nick(new_nick_str);
    }

    fn unlink_user_channel(&mut self, uid: UserId, chid: ChannelId) {
        let should_remove = match self.users.entry(uid) {
            hash_map::Entry::Occupied(mut entry) => {
                if entry.get().channels.len() == 1 && entry.get().channels.contains(&chid) {
                    true
                } else {
                    entry.get_mut().channels.remove(&chid);
                    false
                }
            }
            hash_map::Entry::Vacant(_) => panic!("Inconsistent state")
        };
        if should_remove {
            warn!("removing {:?}", uid);
            self.remove_user_by_id(uid);
        }

        let should_remove = match self.channels.entry(chid) {
            hash_map::Entry::Occupied(mut entry) => {
                if entry.get().users.len() == 1 && entry.get().users.contains(&uid) {
                    true
                } else {
                    entry.get_mut().users.remove(&uid);
                    false
                }
            },
            hash_map::Entry::Vacant(_) => panic!("Inconsistent state")
        };
        if should_remove {
            warn!("removing {:?}", chid);
            self.remove_channel_by_id(chid);
        }
    }
    fn update_channel<F>(&mut self, id: ChannelId, modfunc: F) -> bool where
        F: FnOnce(&mut Channel) -> ()
    {
        match self.channels.entry(id) {
            hash_map::Entry::Occupied(mut entry) => {
                // Channel currently has no indexed mutable state
                modfunc(entry.get_mut());
                true
            }
            hash_map::Entry::Vacant(_) => false
        }
    }

    fn update_channel_by_name<F>(&mut self, name: &str, modfunc: F) -> bool
        where
            F: FnOnce(&mut Channel) -> () {

        let ch_name = IrcIdentifier::from_str(name);
        if let Some(&chan_id) = self.channel_map.get(&ch_name) {
            let result = self.update_channel(chan_id, modfunc);
            self.validate_state_internal_panic();
            result
        } else {
            warn!("Unknown channel name: {:?}", name);
            false
        }
    }

    fn remove_channel_by_name(&mut self, name: &str) -> Option<ChannelId> {
        let ch_name = IrcIdentifier::from_str(name);
        if let Some(&chan_id) = self.channel_map.get(&ch_name) {
            assert!(self.remove_channel_by_id(chan_id));
            self.validate_state_internal_panic();
            Some(chan_id)
        } else {
            warn!("Unknown channel name: {:?}", name);
            None
        }
    }

    fn remove_channel_by_id(&mut self, id: ChannelId) -> bool {
        let (chan_name, users): (_, Vec<_>) = match self.channels.get(&id) {
            Some(chan_state) => (
                IrcIdentifier::from_str(&chan_state.name),
                chan_state.users.iter().map(|x| *x).collect()
            ),
            None => return false
        };
        for user_id in users.into_iter() {
            self.channels.get_mut(&id).unwrap().users.remove(&user_id);
            self.users.get_mut(&user_id).unwrap().channels.remove(&id);
            // self.unlink_user_channel(user_id, id);
        }
        self.channels.remove(&id);
        self.channel_map.remove(&chan_name);
        self.validate_state_internal_panic();
        true
    }

    fn get_channel_by_name(&self, name: &str) -> Option<(ChannelId, &Channel)> {
        let chan_id = match self.channel_map.get(&IrcIdentifier::from_str(name)) {
            Some(chan_id) => *chan_id,
            None => return None
        };
        match self.channels.get(&chan_id) {
            Some(channel) => Some((chan_id, channel)),
            None => panic!("Inconsistent state")
        }
    }

    fn insert_user(&mut self, user: User) {
        let user_id = user.id;
        let nick = IrcIdentifier::from_str(user.prefix.nick().unwrap());
        assert!(self.users.insert(user_id, user).is_none());
        assert!(self.user_map.insert(nick, user_id).is_none());
        self.validate_state_internal_panic();
    }

    fn update_user_by_nick<F>(&mut self, nick: &str, modfunc: F) -> bool where
        F: FnOnce(&mut User) -> ()
    {
        let nick = IrcIdentifier::from_str(nick);
        if let Some(&user_id) = self.user_map.get(&nick) {
            let result = self.update_user(user_id, modfunc);
            self.validate_state_internal_panic();
            result
        } else {
            warn!("Couldn't find user by nick: {:?}", nick);
            false
        }
    }

    fn update_user<F>(&mut self, id: UserId, modfunc: F) -> bool where
        F: FnOnce(&mut User) -> ()
    {
        match self.users.entry(id) {
            hash_map::Entry::Occupied(mut entry) => {
                let prev_nick = IrcIdentifier::from_str(entry.get().prefix.nick().unwrap());
                modfunc(entry.get_mut());
                let new_nick = IrcIdentifier::from_str(entry.get().prefix.nick().unwrap());
                warn!("prev_nick != new_nick || {:?} != {:?}", prev_nick, new_nick);
                if prev_nick != new_nick {
                    warn!("self.user_map -- REMOVE {:?}; INSERT {:?}", prev_nick, new_nick);
                    self.user_map.remove(&prev_nick);
                    self.user_map.insert(new_nick, id);
                }
                true
            }
            hash_map::Entry::Vacant(_) => false
        }
    }

    fn remove_user_by_nick(&mut self, name: &str) -> Option<UserId> {
        let user_id = match self.user_map.get(&IrcIdentifier::from_str(name)) {
            Some(user_id) => *user_id,
            None => return None
        };
        match self.remove_user_by_id(user_id) {
            true => Some(user_id),
            false => panic!("Inconsistent state")
        }
    }

    fn remove_user_by_id(&mut self, id: UserId) -> bool {
        if self.self_id == id {
            panic!("Tried to remove self");
        }
        let (nick, channels): (_, Vec<_>) = match self.users.get(&id) {
            Some(user_state) => (
                IrcIdentifier::from_str(user_state.prefix.nick().unwrap()),
                user_state.channels.iter().map(|x| *x).collect(),
            ),
            None => return false
        };
        for chan_id in channels.into_iter() {
            self.channels.get_mut(&chan_id).unwrap().users.remove(&id);
            self.users.get_mut(&id).unwrap().channels.remove(&chan_id);
        }

        self.users.remove(&id).unwrap();
        self.user_map.remove(&nick).unwrap();
        self.validate_state_internal_panic();
        true
    }

    pub fn identify_channel(&self, chan: &str) -> Option<ChannelId> {
        match self.channel_map.get(&IrcIdentifier::from_str(chan)) {
            Some(chan_id) => Some(chan_id.clone()),
            None => None
        }
    }

    pub fn resolve_channel(&self, chid: ChannelId) -> Option<&Channel> {
        self.channels.get(&chid)
    }

    pub fn identify_nick(&self, nick: &str) -> Option<UserId> {
        match self.user_map.get(&IrcIdentifier::from_str(nick)) {
            Some(user_id) => Some(*user_id),
            None => None
        }
    }

    pub fn resolve_user(&self, uid: UserId) -> Option<&User> {
        self.users.get(&uid)
    }

    pub fn clone_frozen(&self) -> FrozenState {
        FrozenState(self.clone())
    }
}

#[cfg(not(test))]
impl State {
    fn validate_state_internal_panic(&mut self) {
    }
}

#[cfg(test)]
impl State {
    fn validate_state_internal_panic(&mut self) {
        match self.validate_state_internal() {
            Ok(()) => (),
            Err(msg) => panic!("invalid state: {:?}, dump = {:?}", msg, self)
        };
    }

    fn validate_state_internal(&self) -> Result<(), String> {
        for (&id, state) in self.channels.iter() {
            if id != state.id {
                return Err(format!("{:?} at channels[{:?}]", state.id, id));
            }
            for &user_id in state.users.iter() {
                if let Some(user_state) = self.users.get(&user_id) {
                    if !user_state.channels.contains(&id) {
                        return Err(format!("{0:?} ref {1:?} => {1:?} ref {0:?} not holding", id, user_id));
                    }
                } else {
                    return Err(format!("{:?} refs non-existent {:?}", id, user_id));
                }
            }
        }
        for (&id, state) in self.users.iter() {
            if id != state.id {
                return Err(format!("{:?} at users[{:?}]", state.id, id));
            }
            for &chan_id in state.channels.iter() {
                if let Some(chan_state) = self.channels.get(&chan_id) {
                    if !chan_state.users.contains(&id) {
                        return Err(format!("{0:?} ref {1:?} => {1:?} ref {0:?} not holding", id, chan_id));
                    }
                } else {
                    return Err(format!("{:?} refs non-existent {:?}", id, chan_id));
                }
            }
        }
        for (name, &id) in self.channel_map.iter() {
            if let Some(state) = self.channels.get(&id) {
                if *name != IrcIdentifier::from_str(&state.name) {
                    return Err(format!("{:?} at channel_map[{:?}]", state.id, name));
                }
            } else {
                return Err(format!("channel map inconsistent"));
            }
        }
        for (name, &id) in self.user_map.iter() {
            if let Some(state) = self.users.get(&id) {
                if *name != IrcIdentifier::from_str(state.get_nick()) {
                    return Err(format!("{:?} at user_map[{:?}]", state.id, name));
                }
            } else {
                return Err(format!(
                    concat!(
                        "user map inconsistent: self.user_map[{:?}] is not None ",
                        "=> self.users[{:?}] is not None"
                    ), name, id));
            }
        }
        Ok(())
    }
}

impl Eq for State {}

impl PartialEq for State {
    fn eq(&self, other: &State) -> bool {
        if self.user_map != other.user_map {
            return false;
        }
        if self.channel_map != other.channel_map {
            return false;
        }
        if self.users != other.users {
            return false;
        }
        if self.channels != other.channels {
            return false;
        }
        if self.user_seq != other.user_seq {
            return false;
        }
        if self.channel_seq != other.channel_seq {
            return false;
        }
        if self.self_nick != other.self_nick {
            return false;
        }
        if self.generation != other.generation {
            return false;
        }
        return true;
    }
}
