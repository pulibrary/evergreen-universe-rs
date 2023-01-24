use super::session::Session;
use chrono::prelude::*;
use chrono::DateTime;
use json::JsonValue;
use super::conf;

const JSON_NULL: JsonValue = JsonValue::Null;
const DEFAULT_LIST_ITEM_SIZE: usize = 10;

pub enum SummaryListType {
    HoldItems,
    UnavailHoldItems,
    ChargedItems,
    FineItems,
}

pub struct SummaryListOptions {
    list_type: SummaryListType,
    start_item: Option<usize>,
    end_item: Option<usize>,
}

impl SummaryListOptions {
    pub fn list_type(&self) -> &SummaryListType {
        &self.list_type
    }

    /// Returns zero-based offset from 1-based SIP "start item" value.
    pub fn offset(&self) -> usize {
        if let Some(s) = self.start_item {
            if s > 0 { s - 1 } else { 0 }
        } else {
            0
        }
    }

    /// Returns zero-based limit from 1-based SIP "end item" value.
    pub fn limit(&self) -> usize {
        if let Some(e) = self.end_item {
            if e > 0 { e - 1 } else { DEFAULT_LIST_ITEM_SIZE }
        } else {
            DEFAULT_LIST_ITEM_SIZE
        }
    }
}

#[derive(Debug)]
pub struct Patron {
    pub id: i64,
    pub barcode: String,
    pub charge_denied: bool,
    pub renew_denied: bool,
    pub recall_denied: bool,
    pub holds_denied: bool,
    pub card_lost: bool,
    pub max_overdue: bool,
    pub max_fines: bool,
    pub recall_overdue: bool,
    pub max_bills: bool,
    pub valid: bool,
    pub card_active: bool,
    pub balance_owed: f64,
    pub password_verified: bool,
    pub recall_count: i64,
    pub hold_ids: Vec<i64>,
    pub unavail_hold_ids: Vec<i64>,
    pub holds_count: usize,
    pub unavail_holds_count: usize,
    pub items_overdue_count: usize,
    pub items_out_count: usize,
    pub items_overdue_ids: Vec<i64>,
    pub items_out_ids: Vec<i64>,
    pub fine_count: usize,
    pub hold_items: Vec<String>,
}

impl Patron {
    pub fn new(barcode: &str) -> Patron {
        Patron {
            id: 0,
            barcode: barcode.to_string(),
            charge_denied: false,
            renew_denied: false,
            recall_denied: false,
            holds_denied: false,
            card_lost: false,
            max_overdue: false,
            max_fines: false,
            recall_overdue: false,
            max_bills: false,
            valid: false,
            card_active: false,
            balance_owed: 0.0,
            password_verified: false,
            recall_count: 0,
            holds_count: 0,
            unavail_holds_count: 0,
            items_overdue_count: 0,
            items_out_count: 0,
            hold_ids: Vec::new(),
            unavail_hold_ids: Vec::new(),
            items_overdue_ids: Vec::new(),
            items_out_ids: Vec::new(),
            hold_items: Vec::new(),
            fine_count: 0,
        }
    }
}

impl Session {

    pub fn get_patron_details(
        &mut self,
        barcode: &str,
        password_op: Option<&str>,
        summary_list_options: Option<&SummaryListOptions>,
    ) -> Result<Option<Patron>, String> {

        // Make sure we have an authtoken here so we don't have to
        // keep checking for expired sessions during our data collection.
        self.set_authtoken()?;

        let user = match self.get_user(barcode)? {
            Some(u) => u,
            None => return Ok(None),
        };

        let username = user["usrname"].as_str().unwrap(); // required

        let mut patron = Patron::new(barcode);

        patron.id = self.parse_id(&user["id"])?;
        patron.password_verified = self.check_password(&username, password_op)?;

        if let Some(summary) = self.editor_mut().retrieve("mous", patron.id)? {
            patron.balance_owed = self.parse_float(&summary["balance_owed"])?;
        }

        self.set_patron_privileges(&user, &mut patron)?;
        self.set_patron_summary_items(&user, &mut patron)?;

        if let Some(ops) = summary_list_options {
            self.set_patron_summary_list_items(&user, &mut patron, ops)?;
        }

        //
        // TODO
        // self.log_activity

        Ok(Some(patron))
    }

    /// Caller wants to see specific values of a given type, e.g. list
    /// of holds for a patron.
    fn set_patron_summary_list_items(
        &mut self,
        user: &JsonValue,
        patron: &mut Patron,
        summary_ops: &SummaryListOptions
    ) -> Result<(), String> {


        match summary_ops.list_type() {
            SummaryListType::HoldItems => self.add_hold_items(patron, summary_ops, false)?,
            SummaryListType::UnavailHoldItems => self.add_hold_items(patron, summary_ops, true)?,
            SummaryListType::ChargedItems => {}
            SummaryListType::FineItems => {}
        }

        Ok(())
    }

    fn get_data_range(&self,
        summary_ops: &SummaryListOptions, values: &Vec<String>) -> Vec<String> {

        let limit = summary_ops.limit();
        let offset = summary_ops.offset();

        let mut new_values: Vec<String> = Vec::new();

        for idx in offset..(offset + limit) {
            if let Some(v) = values.get(idx) {
                new_values.push(v.to_string());
            }
        }

        new_values
    }

    /// Collect details on holds.
    fn add_hold_items(
        &mut self,
        patron: &mut Patron,
        summary_ops: &SummaryListOptions,
        unavail: bool
    ) -> Result<(), String> {

        let format = self.account().unwrap().settings().msg64_hold_datatype().clone();

        let hold_ids = match unavail {
            true => &patron.unavail_hold_ids,
            false => &patron.hold_ids,
        };

        let mut hold_items: Vec<String> = Vec::new();

        for hold_id in hold_ids {
            if let Some(hold) = self.editor_mut().retrieve("ahr", *hold_id)? {
                if format == conf::Msg64HoldDatatype::Barcode {
                    if let Some(copy) = self.find_copy_for_hold(&hold)? {
                        hold_items.push(copy["barcode"].as_str().unwrap().to_string());
                    }
                } else {
                    if let Some(title) = self.find_title_for_hold(&hold)? {
                        hold_items.push(title);
                    }
                }
            }
        }

        patron.hold_items = self.get_data_range(summary_ops, &hold_items);

        Ok(())
    }

    fn find_title_for_hold(&mut self, hold: &JsonValue) -> Result<Option<String>, String> {

        let hold_id = self.parse_id(&hold["id"])?;
        let bib_link = match self.editor_mut().retrieve("rhrr", hold_id)? {
            Some(l) => l,
            None => return Ok(None), // shouldn't be happen-able
        };

        let bib_id = self.parse_id(&bib_link["bib_record"])?;
        let search = json::object! {
            source: bib_id,
            name: "title",
        };

        let title_fields = self.editor_mut().search("mfde", search)?;

        if let Some(tf) = title_fields.get(0) {
            if let Some(v) = tf["value"].as_str() {
                return Ok(Some(v.to_string()))
            }
        }

        Ok(None)
    }

    fn find_copy_for_hold(&mut self, hold: &JsonValue) -> Result<Option<JsonValue>, String> {

        if !hold["current_copy"].is_null() {
            // We have a captured copy.  Use it.
            let copy_id = self.parse_id(&hold["current_copy"])?;
            return self.editor_mut().retrieve("acp", copy_id);
        }

        let hold_type = hold["hold_type"].as_str().unwrap(); // required
        let hold_target = self.parse_id(&hold["target"])?;

        if hold_type.eq("C") || hold_type.eq("R") || hold_type.eq("F") {
            // These are all copy-level hold types
            return self.editor_mut().retrieve("acp", hold_target);
        }

        if hold_type.eq("V") {
            // For call number holds, any copy will do.
            return self.get_copy_for_vol(hold_target);
        }

        let mut bre_ids: Vec<i64> = Vec::new();

        if hold_type.eq("M") {
            let search = json::object! { metarecord: hold_target };
            let maps = self.editor_mut().search("mmrsm", search)?;
            for map in maps {
                bre_ids.push(self.parse_id(&map["record"])?);
            }
        } else {
            bre_ids.push(hold_target);
        }

        let query = json::object! {
            select: {acp: ["id"]},
            from: {acp: "acn"},
            where: {
                "+acp": {deleted: "f"},
                "+acn": {record: bre_ids, deleted: "f"}
            },
            limit: 1
        };

        let copy_id_hashes = self.editor_mut().json_query(query)?;
        if copy_id_hashes.len() > 0 {
            let copy_id = self.parse_id(&copy_id_hashes[0]["id"])?;
            return self.editor_mut().retrieve("acp", copy_id);
        }

        Ok(None)
    }

    fn get_copy_for_vol(&mut self, vol_id: i64) -> Result<Option<JsonValue>, String> {
        let search = json::object! {
            call_number: vol_id,
            deleted: "f",
        };

        let ops = json::object! { limit: 1usize };

        let copies = self.editor_mut().search_with_ops("acp", search, ops)?;

        if copies.len() == 1 {
            return Ok(Some(copies[0].to_owned()));
        } else {
            return Ok(None)
        }
    }


    fn set_patron_summary_items(
        &mut self,
        user: &JsonValue,
        patron: &mut Patron,
    ) -> Result<(), String> {

        self.set_patron_hold_ids(patron, false, None, None)?;
        self.set_patron_hold_ids(patron, true, None, None)?;

        if let Some(summary) = self.editor_mut().retrieve("ocirclist", patron.id)? {

            // overdue and out are packaged as comma-separated ID values.
            let overdue: Vec<i64> = summary["overdue"]
                .as_str()
                .unwrap()
                .split(",")
                .map(|id| id.parse::<i64>().unwrap())
                .filter(|id| id > &0)
                .collect();

            let outs: Vec<i64> = summary["out"]
                .as_str()
                .unwrap()
                .split(",")
                .map(|id| id.parse::<i64>().unwrap())
                .filter(|id| id > &0)
                .collect();

            patron.items_overdue_count = overdue.len();
            patron.items_out_count = outs.len();
            patron.items_overdue_ids = overdue;
            patron.items_out_ids = outs;
        }

        let search = json::object! {
            usr: patron.id,
            balance_owed: {"<>": 0},
            total_owed: {">": 0},
        };

        let summaries = self.editor_mut().search("mbts", search)?;
        patron.fine_count = summaries.len();

        Ok(())
    }

    fn set_patron_hold_ids(
        &mut self,
        patron: &mut Patron,
        unavail: bool,
        limit: Option<usize>,
        offset: Option<usize>
    ) -> Result<(), String> {

        let mut search = json::object! {
            usr: patron.id,
            fulfillment_time: JSON_NULL,
            cancel_time: JSON_NULL,
        };

        if unavail {
            search["-or"] = json::array! [
              {current_shelf_lib: JSON_NULL},
              {current_shelf_lib: {"!=": {"+ahr": "pickup_lib"}}}
            ];
        } else if self.account().unwrap().settings().msg64_hold_items_available() {
            search["current_shelf_lib"] = json::object! {"=": {"+ahr": "pickup_lib"}};

        }

        let mut query = json::object! {
            select: {ahr: ["id"]},
            from: "ahr",
            where: {"+ahr": search},
        };

        if let Some(l) = limit { query["limit"] = json::from(l); }
        if let Some(o) = offset { query["offset"] = json::from(o); }

        let id_hash_list = self.editor_mut().json_query(query)?;

        for hash in id_hash_list {
            let hold_id = self.parse_id(&hash["id"])?;
            if unavail {
                patron.unavail_hold_ids.push(hold_id);
            } else {
                patron.hold_ids.push(hold_id);
            }
        }

        if unavail {
            patron.unavail_holds_count = patron.unavail_hold_ids.len();
        } else {
            patron.holds_count = patron.hold_ids.len();
        }

        Ok(())
    }

    fn set_patron_privileges(
        &mut self,
        user: &JsonValue,
        patron: &mut Patron,
    ) -> Result<(), String> {
        let expire_date_str = user["expire_date"].as_str().unwrap(); // required

        // chrono has a parse_from_rfc3339() function, but it does
        // not precisely match the format returned by PG, which uses
        // timezone without colons.
        let expire_date = DateTime::parse_from_str(&expire_date_str, "%Y-%m-%dT%H:%M:%S%z")
            .or_else(|e| Err(format!("Invalid expire date: {e} {expire_date_str}")))?;

        if expire_date < Local::now() {
            // Patron is expired.  Don't bother checking other penalties, etc.

            patron.charge_denied = true;
            patron.renew_denied = true;
            patron.recall_denied = true;
            patron.holds_denied = true;

            return Ok(());
        }

        if self
            .account()
            .unwrap()
            .settings()
            .patron_status_permit_all()
        {
            // This setting group allows all patron actions regardless
            // of penalties, fines, etc.
            return Ok(());
        }

        let penalties = self.get_patron_penalties(patron.id)?;

        patron.max_fines = self.penalties_contain(1, &penalties)?; // PATRON_EXCEEDS_FINES
        patron.max_overdue = self.penalties_contain(2, &penalties)?; // PATRON_EXCEEDS_OVERDUE_COUNT
        patron.card_active = self.parse_bool(&user["card"]["active"]);

        let blocked =
             self.parse_bool(&user["barred"]) ||
            !self.parse_bool(&user["active"]) ||
            !patron.card_active;

        let mut block_tags = String::new();
        for pen in penalties.iter() {
            if let Some(tag) = pen["block_tag"].as_str() {
                block_tags += tag;
            }
        }

        if !blocked && block_tags.len() == 0 {
            // No blocks, etc. left to inspect.  All done.
            return Ok(())
        }

        patron.holds_denied = blocked || block_tags.contains("HOLDS");

        if self.account().unwrap().settings().patron_status_permit_loans() {
            // We're going to ignore checkout, renewals blocks for now.
            return Ok(())
        }

        patron.charge_denied = blocked || block_tags.contains("CIRC");
        patron.renew_denied = blocked || block_tags.contains("RENEW");

		// In evergreen, patrons cannot create Recall holds directly, but that
		// doesn't mean they would not have said privilege if the functionality
		// existed.  Base the ability to perform recalls on whether they have
		// checkout and holds privilege, since both would be needed for recalls.
		patron.recall_denied = patron.charge_denied || patron.renew_denied;

        Ok(())
    }

    fn penalties_contain(&self, penalty_id: i64, penalties: &Vec<JsonValue>) -> Result<bool, String> {
        for pen in penalties.iter() {
            let pen_id = self.parse_id(&pen["id"])?;
            if pen_id == penalty_id {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn get_patron_penalties(&mut self, user_id: i64) -> Result<Vec<JsonValue>, String> {

        let requestor = self.editor().requestor().unwrap();

        let mut field = &requestor["ws_ou"];
        if field.is_null() {
            field = &requestor["home_ou"];
        };
        let ws_org = self.parse_id(field)?;

        let search = json::object! {
            select: {csp: ["id", "block_list"]},
            from: {ausp: "csp"},
            where: {
                "+ausp": {
                    usr: user_id,
                    "-or": [
                      {stop_date: JSON_NULL},
                      {stop_date: {">": "now"}},
                    ],
                    org_unit: {
                        in: {
                            select: {
                                aou: [{
                                    transform: "actor.org_unit_full_path",
                                    column: "id",
                                    result_field: "id",
                                }]
                            },
                            from: "aou",
                            where: {id: ws_org}
                        }
                    }
                }
            }
        };

        self.editor_mut().json_query(search)
    }

    fn get_user(&mut self, barcode: &str) -> Result<Option<JsonValue>, String> {
        let search = json::object! { barcode: barcode };

        let flesh = json::object! {
            flesh: 3,
            flesh_fields: {
                ac: ["usr"],
                au: ["billing_address", "mailing_address", "profile", "stat_cat_entries"],
                actscecm: ["stat_cat"]
            }
        };

        let mut cards = self.editor_mut().search_with_ops("ac", search, flesh)?;

        if cards.len() == 0 {
            return Ok(None);
        }

        let mut user = cards[0]["usr"].take();
        user["card"] = cards[0].to_owned();

        Ok(Some(user))
    }

    fn check_password(
        &mut self,
        username: &str,
        password_op: Option<&str>,
    ) -> Result<bool, String> {
        let password = match password_op {
            Some(p) => p,
            None => return Ok(false),
        };

        let mut ses = self.osrf_client_mut().session("open-ils.actor");

        let mut req = ses.request(
            "open-ils.actor.verify_user_password",
            vec![
                json::from(self.authtoken()?),
                JSON_NULL,
                json::from(username),
                JSON_NULL,
                json::from(password),
            ],
        )?;

        if let Some(resp) = req.recv(60)? {
            Ok(self.parse_bool(&resp))
        } else {
            Err(format!("API call timed out"))
        }
    }

    pub fn handle_patron_status(&mut self, msg: &sip2::Message) -> Result<sip2::Message, String> {
        let barcode = msg
            .get_field_value("AA")
            .ok_or(format!("handle_patron_status() missing patron barcode"))?;

        let password_op = msg.get_field_value("AD"); // optional

        let patron_op = self.get_patron_details(&barcode, password_op.as_deref(), None)?;
        self.patron_response_common("24", &barcode, patron_op.as_ref())
    }

    fn patron_response_common(&self, msg_code: &str,
        barcode: &str, patron_op: Option<&Patron>) -> Result<sip2::Message, String> {

        let msg_spec = sip2::spec::Message::from_code(msg_code)
            .ok_or(format!("Invalid SIP message code: {msg_code}"))?;

        if patron_op.is_none() {

            let status = format!("{}{}{}{}          ",
                sip2::util::space_bool(false),
                sip2::util::space_bool(false),
                sip2::util::space_bool(false),
                sip2::util::space_bool(false));

            let mut resp = sip2::Message::new(
                msg_spec,
                vec![
                    sip2::FixedField::new(&sip2::spec::FF_PATRON_STATUS, &status).unwrap(),
                    sip2::FixedField::new(&sip2::spec::FF_LANGUAGE, "000").unwrap(),
                    sip2::FixedField::new(&sip2::spec::FF_DATE, &sip2::util::sip_date_now()).unwrap(),
                ],
                Vec::new(),
            );

            resp.add_field("AO", self.account().unwrap().settings().institution());
            resp.add_field("AA", barcode);
            resp.add_field("BL", sip2::util::sip_bool(false)); // valid patron
            resp.add_field("CQ", sip2::util::sip_bool(false)); // valid patron password

            return Ok(resp)
        }

        let patron = patron_op.unwrap();

        log::debug!("PATRON: {patron:?}");

        let status = format!("{}{}{}{}{}{}{}{}{}{}{}{}{}{}",
            sip2::util::space_bool(patron.charge_denied),
            sip2::util::space_bool(patron.renew_denied),
            sip2::util::space_bool(patron.recall_denied),
            sip2::util::space_bool(patron.holds_denied),
            sip2::util::space_bool(patron.card_active),
            sip2::util::space_bool(false), // max charged
            sip2::util::space_bool(patron.max_overdue),
            sip2::util::space_bool(false), // max renewals
            sip2::util::space_bool(false), // max claims returned
            sip2::util::space_bool(false), // max lost
            sip2::util::space_bool(patron.max_fines),
            sip2::util::space_bool(patron.max_fines),
            sip2::util::space_bool(false), // recall overdue
            sip2::util::space_bool(patron.max_fines));

        let mut resp = sip2::Message::new(
            msg_spec,
            vec![
                sip2::FixedField::new(&sip2::spec::FF_PATRON_STATUS, &status).unwrap(),
                sip2::FixedField::new(&sip2::spec::FF_LANGUAGE, "000").unwrap(),
                sip2::FixedField::new(&sip2::spec::FF_DATE, &sip2::util::sip_date_now()).unwrap(),
            ],
            Vec::new(),
        );

        resp.add_field("AA", barcode);
        resp.add_field("AO", self.account().unwrap().settings().institution());
        resp.add_field("BH", self.sip_config().currency());
        resp.add_field("BL", sip2::util::sip_bool(true)); // valid patron
        resp.add_field("BV", &format!("{:.2}", patron.balance_owed));
        resp.add_field("CQ", sip2::util::sip_bool(patron.password_verified));

        Ok(resp)
    }
}
