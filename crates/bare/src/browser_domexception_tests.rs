use crate::browser_fetch_tests::{big, js, mk};

#[test]
fn domexception_converts_constructor_arguments() {
    big(|| {
        let mut b = mk(None, Default::default());
        assert_eq!(js(&mut b, "var e = new DOMException(1,2); [e.name,e.message,e.code].join()"), "2,1,0");
    });
}

#[test]
fn domexception_retained_polyfill_probe() {
    big(|| {
        let mut b = mk(None, Default::default());
        assert_eq!(js(&mut b, "var y = globalThis.DOMException; var C = 'stack' in new y(1,2); typeof C"), "boolean");
    });
}

#[test]
fn domexception_defaults_identity_codes_and_constants() {
    big(|| {
        let mut b = mk(None, Default::default());
        assert_eq!(js(&mut b, "var e = new DOMException(); [e.message,e.name,e.code,e instanceof DOMException,e instanceof Error,DOMException.length,DOMException.prototype.constructor===DOMException].join()"), ",Error,0,true,true,0,true");
        assert_eq!(js(&mut b, "var e = new DOMException(undefined,undefined); [e.message,e.name].join()"), ",Error");
        assert_eq!(js(&mut b, "[new DOMException('', 'NetworkError').code,DOMException.NETWORK_ERR,DOMException.prototype.NETWORK_ERR,new DOMException('', 'unknown').code].join()"), "19,19,19,0");
        assert_eq!(js(&mut b, "[new DOMException('','DOMStringSizeError').code,new DOMException('','NoDataAllowedError').code,new DOMException('','ValidationError').code].join()"), "0,0,0");
        assert_eq!(js(&mut b, "Object.prototype.toString.call(new DOMException())"), "[object DOMException]");
        assert_eq!(js(&mut b, "try { DOMException() } catch(e) { e.name }"), "TypeError");
        assert_eq!(js(&mut b, "class D extends DOMException {}; var d = new D('x','AbortError'); [d instanceof D,d instanceof DOMException,d.message,d.code].join()"), "true,true,x,20");
    });
}

#[test]
fn domexception_conversion_and_utf16() {
    big(|| {
        let mut b = mk(None, Default::default());
        assert_eq!(js(&mut b, "var order=''; var e=new DOMException({toString(){order+='m';return 'msg'}},{toString(){order+='n';return 'SyntaxError'}}); [order,e.message,e.name,e.code].join()"), "mn,msg,SyntaxError,12");
        assert_eq!(js(&mut b, "try { new DOMException(Symbol()) } catch(e) { e.name }"), "TypeError");
        assert_eq!(js(&mut b, "try { new DOMException({toString(){throw 42}}) } catch(e) { e }"), "42");
        assert_eq!(js(&mut b, "var e=new DOMException('\\ud800','\\udfff'); [e.message.charCodeAt(0),e.name.charCodeAt(0),e.code].join()"), "55296,57343,0");
    });
}

#[test]
fn domexception_enumeration_descriptors_and_host_errors() {
    big(|| {
        let mut b = mk(None, Default::default());
        assert_eq!(js(&mut b, "Object.getOwnPropertyNames(globalThis).includes('DOMException')"), "true");
        assert_eq!(js(&mut b, "var C=DOMException; var e=new C('x','SyntaxError'); [C===DOMException,e.toString(),Object.getOwnPropertyDescriptor(e,'name').writable,Object.getOwnPropertyDescriptor(e,'name').enumerable].join()"), "true,SyntaxError: x,true,false");
        assert_eq!(js(&mut b, "try { document.removeChild(document.createElement('div')) } catch(e) { [e instanceof DOMException,e instanceof Error].join() }"), "true,true");
    });
}

#[test]
fn domexception_is_created_only_on_first_use_and_rooted() {
    big(|| {
        let mut b = mk(None, Default::default());
        let g = b.it.global_object();
        let k = b.it.heap.key_from_str("DOMException").unwrap();
        assert!(b.it.heap.peek_own_property(g,k).unwrap().is_none());
        let n = b.dom.handlers.borrow().len();
        assert_eq!(js(&mut b,"1 + 1"),"2");
        assert_eq!(b.dom.handlers.borrow().len(), n);
        assert_eq!(js(&mut b,"var ctor = DOMException; typeof ctor"),"function");
        assert_eq!(b.dom.handlers.borrow().len(), n + 1);
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b,"ctor === DOMException && new ctor() instanceof ctor"),"true");
        assert_eq!(b.dom.handlers.borrow().len(), n + 1);
    });
}

#[test]
fn domexception_respects_host_string_limit() {
    big(|| {
        let mut b = crate::browser::Browser::new(bare_html::tree::parse("<body>"), crate::browser::BrowserLimits { max_string_bytes: 128, ..Default::default() }, Default::default(), Default::default()).unwrap();
        assert!(js(&mut b, "new DOMException('x'.repeat(129))").starts_with("limit:"));
    });
}
