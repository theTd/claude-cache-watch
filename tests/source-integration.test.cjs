const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

function harness() {
  const elements=new Map(), requests=[], subscribers=[], timers=new Map();let timer=0;
  function node(id='') {
    return {id,value:'',checked:false,hidden:false,textContent:'',children:[],handlers:{},dataset:{},
      classList:{add(){},toggle(){}},setAttribute(){},querySelectorAll(){return[];},
      append(...children){this.children.push(...children);},replaceChildren(...children){this.children=children;},
      addEventListener(event,fn){this.handlers[event]=fn;}};
  }
  const element=id=>{if(!elements.has(id))elements.set(id,node(id));return elements.get(id);};
  const context=vm.createContext({Intl,Date,Set,console,
    document:{getElementById:element,createElement:()=>node(),querySelectorAll:()=>[]},
    window:{__TAURI__:{core:{invoke:async(command,args)=>{
      requests.push({command,args});
      if(command==='snapshot')return {settings:{source:'C:/example',sources:[],threshold:100000},status:{ready:true},history:{rows:[],total:0}};
      if(command==='statistics')return {models:[],calls:0,priced_calls:0,undated_calls:0};
      if(command==='pricing_catalog')return [];
      return 'example-export.json';
    }}},dashboardRefresh:{active:true,subscribe(fn){subscribers.push(fn);}}},
    setTimeout(fn){const id=++timer;timers.set(id,fn);return id;},clearTimeout(id){timers.delete(id);},
  });
  for(const file of ['app.js','interval.js','statistics.js'])vm.runInContext(fs.readFileSync(path.join(__dirname,'../web',file),'utf8'),context);
  return {element,context,requests,async refresh(){await vm.runInContext('refresh()',context);},async flush(){for(const fn of timers.values())fn();timers.clear();await new Promise(setImmediate);}};
}
test('source and call-type filters reach history, statistics and JSON export together',async()=>{
  const h=harness();h.element('source-filter').value='antigravity_cli';h.element('kind-filter').value='checkpoint';
  h.element('source-filter').handlers.change();await new Promise(setImmediate);await h.flush();
  await h.element('export').handlers.click();
  for(const command of ['snapshot','statistics','export_history_page']) {
    const request=h.requests.find(r=>r.command===command);assert.ok(request,command);
    assert.deepEqual(JSON.parse(JSON.stringify(request.args.filter)),{source:'antigravity_cli',kind:'checkpoint'});
  }
});
test('source settings preserve separate toggles and paths',async()=>{
  const h=harness();
  for(const [kind,p]of [['claude','C:/claude'],['antigravity_ide','C:/ide'],['antigravity_cli','C:/cli']]) {
    h.element(kind==='claude'?'source':'source-'+kind).value=p;h.element('enabled-'+kind).checked=kind!=='claude';
  }
  h.element('threshold').value=234567;h.element('paused').checked=true;
  await h.element('settings-form').handlers.submit({preventDefault(){}});
  const settings=h.requests.find(r=>r.command==='save_settings').args.settings;
  assert.equal(settings.threshold,234567);assert.equal(settings.paused,true);assert.equal(settings.sources.length,3);
  assert.equal(settings.sources[0].enabled,false);assert.equal(settings.sources[1].path,'C:/ide');
});
test('unknown cache/write usage and excluded costs are never displayed as zero',()=>{
  const h=harness();
  const row={timestamp:'2026-11-09',source_kind:'antigravity_ide',kind:'checkpoint',state:'DONE',
    session_title:'Example',input:262543,creation:0,read:0,output:6341,usage_complete:true,cache_known:false,creation_known:false,
    id:'example',estimated_billing:{total_usd:1,input_usd:1,cache_write_usd:null,cache_read_usd:0,output_usd:0,web_search_usd:null,rates_per_million:{},basis:'Reference',notes:[]}};
  h.context.exampleRow=row;
  const rendered=vm.runInContext('renderRow(exampleRow,new Set())',h.context);
  assert.equal(rendered.children[3].textContent,'Unknown');assert.equal(rendered.children[4].textContent,'Unknown');
  const breakdown=rendered.children[6].children[0].children[1].textContent;
  assert.match(breakdown,/Cache writes Unknown/);assert.match(breakdown,/Web search Unreported/);
});
