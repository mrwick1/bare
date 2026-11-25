use super::*;

#[test]
fn worker_can_queue_messages_for_another_agents_port() {
    let hub = Arc::new(PortHub::default());
    let end = hub.new_end(Some(0));
    let worker = hub.clone();
    std::thread::spawn(move || {
        for n in [1, 2] {
            let cloned = Cloned::new(Box::new([n]), Vec::new(), 1);
            assert!(worker.push(end, Wire { cloned, bytes: 17, origin: None }, 2, 34).is_ok());
        }
        let cloned = Cloned::new(Box::new([3]), Vec::new(), 1);
        let result = worker.push(end, Wire { cloned, bytes: 17, origin: None }, 2, 34);
        assert!(matches!(result, Err((_, Some("messages")))));
    }).join().unwrap();
    assert_eq!((hub.wires(), hub.bytes()), (2, 34));
    assert_eq!(hub.pop(end).unwrap().cloned.bytes(), &[1]);
    assert_eq!(hub.pop(end).unwrap().cloned.bytes(), &[2]);
    assert_eq!((hub.wires(), hub.bytes()), (0, 0));
}
