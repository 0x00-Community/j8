mod collator;
mod datetime;
mod locale;
mod number;
mod plural;
mod service;
mod zone;

use alloc::string::String;
use alloc::vec::Vec;

use j8framework::{Error, ErrorKind, JsObject, NativeClosure, ObjectData, ObjectKind, Property, Scope, Slot, Value};

use crate::{Args, Installer, LocaleJoin};
use collator::{Cases, Collation, Collators};
use datetime::{DateTimeFormat, DateTimeFormats};
use locale::Tags;
use number::{NumberFormat, NumberFormats};
use plural::PluralRulesBuiltins;

struct Intl;

impl Intl {
    fn get_canonical_locales<'scope>(a: &mut Scope<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let list: Vec<String> = Tags::list(a, args.at(0))?.iter().map(alloc::string::ToString::to_string).collect();
        Ok(Tags::strings(a, &list))
    }

    fn array_to_locale_string<'scope>(a: &mut Scope<'scope, '_>, this: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let o = a.to_object(this)?;
        let len = a.length_of_array_like(o.into())?;
        LocaleJoin::join(a, o, len, args)
    }

    fn supported_values_of<'scope>(a: &mut Scope<'scope, '_>, _: Value<'scope>, args: &[Value<'scope>]) -> Result<Value<'scope>, Error> {
        let key = a.to_string(args.at(0))?;
        let mut sorted: Vec<String> = match key.as_str() {
            "calendar" => DateTimeFormat::calendars().iter().map(|s| String::from(*s)).collect(),
            "collation" => Collation::types().iter().map(|s| String::from(*s)).collect(),
            "numberingSystem" => NumberFormat::numbering_systems().iter().map(|s| String::from(*s)).collect(),
            "unit" => NumberFormat::units().iter().map(|s| String::from(*s)).collect(),
            "timeZone" => zone::Zone::names(),
            "currency" => Vec::new(),
            _ => return Err(a.throw(ErrorKind::Range, &alloc::format!("Invalid key : {}", key.as_str()))),
        };
        sorted.sort();
        Ok(Tags::strings(a, &sorted))
    }
}

impl<'scope> Installer<'_, 'scope> {
    fn service(&self, intl: JsObject<'scope>, name: &str, construct: j8framework::ClosureFn, supported: j8framework::NativeFn, tag: &str) -> JsObject<'scope> {
        let a = self.context;
        let proto = self.plain();
        let closure = NativeClosure { f: construct, slots: alloc::vec![proto.into()], constructor: true };
        let ctor = ObjectData { kind: ObjectKind::NativeClosure(closure), proto: Some(a.function_proto), ..ObjectData::default() }.alloc(self.mc);
        a.name_function(self.mc, ctor, name, Some(0));
        ctor.borrow_mut(self.mc).define("prototype", Property { slot: Slot::Data(proto.into()), enumerable: false, writable: false, configurable: false });
        self.set(proto, "constructor", ctor.into());
        self.set(intl, name, ctor.into());
        self.method(ctor, "supportedLocalesOf", supported, 1);
        let t = self.text(tag);
        proto.borrow_mut(self.mc).define_symbol(a.symbols.to_string_tag, Property { slot: Slot::Data(t), enumerable: false, writable: false, configurable: true });
        proto
    }

    pub(crate) fn intl(&mut self) {
        let r = self.context;
        let intl = self.plain();
        self.set(r.global, "Intl", intl.into());
        let tag = self.text("Intl");
        intl.borrow_mut(self.mc).define_symbol(r.symbols.to_string_tag, Property { slot: Slot::Data(tag), enumerable: false, writable: false, configurable: true });
        self.method(intl, "getCanonicalLocales", Intl::get_canonical_locales, 1);
        self.method(intl, "supportedValuesOf", Intl::supported_values_of, 1);

        let collator = self.service(intl, "Collator", Collators::construct, Collators::supported_locales_of, "Intl.Collator");
        self.getter(collator, "compare", Collators::compare);
        self.method(collator, "resolvedOptions", Collators::resolved_options, 0);

        let number = self.service(intl, "NumberFormat", NumberFormats::construct, NumberFormats::supported_locales_of, "Intl.NumberFormat");
        self.getter(number, "format", NumberFormats::format);
        self.method(number, "formatToParts", NumberFormats::format_to_parts, 1);
        self.method(number, "resolvedOptions", NumberFormats::resolved_options, 0);

        let date = self.service(intl, "DateTimeFormat", DateTimeFormats::construct, DateTimeFormats::supported_locales_of, "Intl.DateTimeFormat");
        self.getter(date, "format", DateTimeFormats::format);
        self.method(date, "formatToParts", DateTimeFormats::format_to_parts, 1);
        self.method(date, "resolvedOptions", DateTimeFormats::resolved_options, 0);

        let plural = self.service(intl, "PluralRules", PluralRulesBuiltins::construct, PluralRulesBuiltins::supported_locales_of, "Intl.PluralRules");
        self.method(plural, "select", PluralRulesBuiltins::select, 1);
        self.method(plural, "selectRange", PluralRulesBuiltins::select_range, 2);
        self.method(plural, "resolvedOptions", PluralRulesBuiltins::resolved_options, 0);

        self.method(r.string_proto, "localeCompare", Collators::locale_compare, 1);
        self.method(r.string_proto, "toLocaleUpperCase", Cases::to_upper, 0);
        self.method(r.string_proto, "toLocaleLowerCase", Cases::to_lower, 0);
        self.method(r.array_proto, "toLocaleString", Intl::array_to_locale_string, 0);
        self.method(r.number_proto, "toLocaleString", NumberFormats::number_to_locale_string, 0);
        self.method(r.bigint_proto, "toLocaleString", NumberFormats::bigint_to_locale_string, 0);
        self.method(r.date_proto, "toLocaleString", DateTimeFormats::to_locale_string, 0);
        self.method(r.date_proto, "toLocaleDateString", DateTimeFormats::to_locale_date_string, 0);
        self.method(r.date_proto, "toLocaleTimeString", DateTimeFormats::to_locale_time_string, 0);
    }
}
