use yrs::branch::{Branch, BranchID};
use yrs::types::xml::{XmlElementPrelim, XmlFragment, XmlTextPrelim};
use yrs::updates::decoder::Decode;
use yrs::{Doc, GetString, ReadTxn, Transact, Update, XmlFragmentRef};

fn bid(b: &impl AsRef<Branch>) -> String {
    match b.as_ref().id() {
        BranchID::Nested(id) => format!("{}:{}", id.client, id.clock),
        BranchID::Root(name) => format!("root:{}", name),
    }
}

fn main() {
    let doc = Doc::new();
    let frag: XmlFragmentRef = doc.get_or_insert_xml_fragment("content");

    let (id_at_create, id_after_edit) = {
        let mut txn = doc.transact_mut();
        let para = frag.push_back(&mut txn, XmlElementPrelim::empty("paragraph"));
        let a = bid(&para);
        para.insert(&mut txn, 0, XmlTextPrelim::new("hello"));
        let b = bid(&para);
        (a, b)
    };
    println!("id at create      = {}", id_at_create);
    println!("id after edit     = {}  (stable: {})", id_after_edit, id_at_create == id_after_edit);

    // Does the SAME block carry the SAME id on a different replica after sync?
    // This is the property block_id actually depends on.
    let remote = Doc::new();
    let _: XmlFragmentRef = remote.get_or_insert_xml_fragment("content");
    let update = doc.transact().encode_state_as_update_v1(&Default::default());
    remote
        .transact_mut()
        .apply_update(Update::decode_v1(&update).unwrap())
        .unwrap();

    let rfrag: XmlFragmentRef = remote.get_or_insert_xml_fragment("content");
    let rtxn = remote.transact();
    let first = rfrag.get(&rtxn, 0).unwrap();
    let remote_id = match first {
        yrs::types::xml::XmlOut::Element(e) => bid(&e),
        _ => "not-an-element".to_string(),
    };
    println!("id on replica B   = {}  (matches: {})", remote_id, remote_id == id_at_create);
    println!("xml               = {}", rfrag.get_string(&rtxn));
}
