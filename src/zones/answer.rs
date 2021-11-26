use domain::base::iana::Rcode;
use domain::base::message::Message;
use domain::base::message_builder::MessageBuilder;
use domain::base::octets::OctetsBuilder;
use super::rrset::{SharedRrset, StoredDname};

//------------ Answer --------------------------------------------------------

#[derive(Clone)]
pub struct Answer {
    /// The response code of the answer.
    rcode: Rcode,

    /// The actual answer, if available.
    answer: Option<SharedRrset>,

    // Cname chain

    /// The optional authority section to be included in the answer.
    authority: Option<AnswerAuthority>
}

impl Answer {
    pub fn new(rcode: Rcode) -> Self {
        Answer {
            rcode,
            answer: None,
            authority: Default::default(),
        }
    }

    pub fn with_authority(rcode: Rcode, authority: AnswerAuthority) -> Self {
        Answer {
            rcode,
            answer: None,
            authority: Some(authority),
        }
    }

    pub fn refused() -> Self {
        Answer::new(Rcode::Refused)
    }

    pub fn add_answer(&mut self, answer: SharedRrset) {
        self.answer = Some(answer.clone())
    }

    pub fn add_authority(&mut self, authority: AnswerAuthority) {
        self.authority = Some(authority)
    }

    pub fn to_message<Target: OctetsBuilder>(
        &self,
        message: Message<&[u8]>,
        builder: MessageBuilder<Target>
    ) -> Target {
        let question = message.sole_question().unwrap();
        let qname = question.qname();
        let qclass = question.qclass();
        let mut builder = builder.start_answer(&message, self.rcode).unwrap();

        if let Some(ref answer) = self.answer {
            for item in answer.data() {
                builder.push((qname, qclass, answer.ttl(), item)).unwrap();
            }
        }

        let mut builder = builder.authority();
        if let Some(authority) = self.authority.as_ref() {
            for item in authority.ns_or_soa.data() {
                builder.push(
                    (
                        authority.owner.clone(), qclass,
                        authority.ns_or_soa.ttl(),
                        item
                    )
                ).unwrap()
            }
            if let Some(ref ds) = authority.ds {
                for item in ds.data() {
                    builder.push(
                        (authority.owner.clone(), qclass, ds.ttl(), item)
                    ).unwrap()
                }
            }
        }

        builder.finish()
    }
}


//------------ AnswerAuthority -----------------------------------------------

/// The authority section of a query answer.
#[derive(Clone)]
pub struct AnswerAuthority {
    /// The owner name of the record sets in the authority section.
    owner: StoredDname,

    /// The NS or SOA record set.
    ///
    /// If the answer is a no-data answer, the SOA record is used. If the
    /// answer is a delegation answer, the NS record is used.
    ns_or_soa: SharedRrset,

    /// The optional DS record set.
    ///
    /// This is only used in a delegation answer.
    ds: Option<SharedRrset>,
}

impl AnswerAuthority {
    pub fn new(
        owner: StoredDname,
        ns_or_soa: SharedRrset,
        ds: Option<SharedRrset>,
    ) -> Self {
        AnswerAuthority { owner, ns_or_soa, ds }
    }
}

