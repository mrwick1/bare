import React, { useState, useEffect, useMemo, useCallback, createContext, useContext } from 'react';
import { createRoot } from 'react-dom/client';
import * as Select from '@radix-ui/react-select';
import * as Accordion from '@radix-ui/react-accordion';
import * as Checkbox from '@radix-ui/react-checkbox';
import * as Tabs from '@radix-ui/react-tabs';
import * as Switch from '@radix-ui/react-switch';
import * as Label from '@radix-ui/react-label';
import * as RadioGroup from '@radix-ui/react-radio-group';

const Form = createContext(null);
window.__c = {};
const cnt = (k) => { window.__c[k] = (window.__c[k] || 0) + 1; };
const N = __N;
const MAX = 500;

function Field({ name, i }) {
  cnt('Field');
  const { v, set } = useContext(Form);
  const val = v[name] || '';
  const bad = val.length > 0 && val.length < 3;
  return (
    <div className={'row ' + (bad ? 'bad' : val ? 'ok' : '')}>
      {__NOLABEL ? <label htmlFor={name} className="lbl">Field {i}</label> : <Label.Root htmlFor={name} className="lbl">Field {i}</Label.Root>}
      <input id={name} name={name} type="text" className="inp" value={val} onChange={(e) => set(name, e.target.value)} />
      {bad ? <span className="err">too short</span> : null}
    </div>
  );
}

function Area({ name, label }) {
  cnt('Area');
  const { v, set } = useContext(Form);
  const val = v[name] || '';
  return (
    <div className="row">
      <Label.Root htmlFor={name} className="lbl">{label}</Label.Root>
      <textarea id={name} name={name} rows={4} className="inp" value={val} onChange={(e) => set(name, e.target.value)} />
      <div className={'count ' + (val.length > MAX - 20 ? 'warn' : '')}>{val.length} / {MAX}</div>
    </div>
  );
}

function Progress() {
  cnt('Progress');
  const { v } = useContext(Form);
  const done = useMemo(() => Object.values(v).filter((x) => x && x.length >= 3).length, [v]);
  useEffect(() => { document.title = done + ' done'; }, [done]);
  return <div className="progress"><div className="bar" style={{ width: (done / (N + 2)) * 100 + '%' }} />{done} of {N + 2}</div>;
}

function Summary() {
  cnt('Summary');
  const { v } = useContext(Form);
  const total = Object.values(v).reduce((a, x) => a + (x ? x.length : 0), 0);
  return <div className="summary">Total characters: {total}</div>;
}

function Choice() {
  cnt('Choice');
  const [sel, setSel] = useState('a');
  const [chk, setChk] = useState(false);
  const [sw, setSw] = useState(false);
  const [rg, setRg] = useState('x');
  return (
    <div className="choices">
      <Select.Root value={sel} onValueChange={setSel}>
        <Select.Trigger className="trg" aria-label="pick"><Select.Value /><Select.Icon>v</Select.Icon></Select.Trigger>
        <Select.Portal><Select.Content className="cnt"><Select.Viewport>
          {['a', 'b', 'c', 'd'].map((o) => <Select.Item key={o} value={o} className="it"><Select.ItemText>Option {o}</Select.ItemText></Select.Item>)}
        </Select.Viewport></Select.Content></Select.Portal>
      </Select.Root>
      <Checkbox.Root className="chk" checked={chk} onCheckedChange={setChk} id="c1"><Checkbox.Indicator>x</Checkbox.Indicator></Checkbox.Root>
      <Switch.Root className="sw" checked={sw} onCheckedChange={setSw}><Switch.Thumb className="thumb" /></Switch.Root>
      <RadioGroup.Root value={rg} onValueChange={setRg}>
        {['x', 'y', 'z'].map((o) => <RadioGroup.Item key={o} value={o} className="rad"><RadioGroup.Indicator>.</RadioGroup.Indicator></RadioGroup.Item>)}
      </RadioGroup.Root>
    </div>
  );
}

function App() {
  cnt('App');
  const [v, setV] = useState({});
  const set = useCallback((k, x) => setV((o) => ({ ...o, [k]: x })), []);
  const names = useMemo(() => Array.from({ length: N }, (_, i) => 'f' + i), []);
  const ctx = useMemo(() => ({ v, set }), [v, set]);
  return (
    <Form.Provider value={ctx}>
      <Progress />
      {__NOCHOICE ? null : <Choice />}
      {__NORADIX ? <div>{names.map((n, j) => <Field key={n} name={n} i={j} />)}</div> : <Tabs.Root defaultValue="t1">
        <Tabs.List><Tabs.Trigger value="t1">One</Tabs.Trigger><Tabs.Trigger value="t2">Two</Tabs.Trigger></Tabs.List>
        <Tabs.Content value="t1">
          <Accordion.Root type="multiple" defaultValue={['s1', 's2', 's3']}>
            {[0, 1, 2].map((s) => (
              <Accordion.Item key={s} value={'s' + (s + 1)}>
                <Accordion.Header><Accordion.Trigger>Section {s + 1}</Accordion.Trigger></Accordion.Header>
                <Accordion.Content>
                  {names.slice(s * 20, s * 20 + 20).map((n, j) => <Field key={n} name={n} i={s * 20 + j} />)}
                </Accordion.Content>
              </Accordion.Item>
            ))}
          </Accordion.Root>
        </Tabs.Content>
        <Tabs.Content value="t2">two</Tabs.Content>
      </Tabs.Root>}
      <Area name="a1" label="About" />
      <Area name="a2" label="Details" />
      <Summary />
    </Form.Provider>
  );
}

createRoot(document.getElementById('root')).render(<App />);
