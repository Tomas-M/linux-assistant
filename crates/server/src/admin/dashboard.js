'use strict';
const state={page:0,userPage:0,users:[],accounts:[],selected:null,report:null,adjustment:null,refreshing:false,reportVersion:0,userVersion:0,filters:null,search:''};
const number=new Intl.NumberFormat();
const dollars=new Intl.NumberFormat(undefined,{style:'currency',currency:'USD',minimumFractionDigits:2,maximumFractionDigits:6});
const markupCache=new WeakMap();
let addingAccount=false;
const logs={before:null,next:null,search:'',level:'',entries:[],version:0};

// Preserve open disclosures and focus when the escaped markup has not changed.
function setMarkup(element,markup)
{
    if (markupCache.get(element)===markup) return false;
    markupCache.set(element,markup);
    element.innerHTML=markup;
    return true;
}

// Escape every server-controlled value before interpolating table markup.
function escapeHtml(value)
{
    return String(value??'').replace(/[&<>"']/g,char=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[char]));
}

// Keep unknown provider measurements visibly distinct from zero.
function count(value) { return value===null || value===undefined ? 'Unknown' : number.format(value); }
// Keep missing usage and very small positive estimates distinct from zero cost.
function money(value)
{
    if (value===null) return 'Unknown';
    if (value>0 && value<.000001) return '<$0.000001';
    return dollars.format(value);
}
function utc(value) { return value ? new Date(value*1000).toISOString().replace('T',' ').slice(0,19)+' UTC' : 'Not yet'; }
function localInput(date) { return new Date(date.getTime()-date.getTimezoneOffset()*60000).toISOString().slice(0,16); }

// All mutations require a same-origin header; session cookies remain inaccessible to JavaScript.
async function api(path,body)
{
    const response=await fetch('/admin/api/'+path,body===undefined ? {} : {method:'POST',headers:{'Content-Type':'application/json','X-Admin-Request':'1'},body:JSON.stringify(body)});
    if (response.status===401) { location.href='/admin/login'; throw new Error('Session expired'); }
    const text=await response.text();
    let data;
    try { data=JSON.parse(text); } catch { throw new Error('Request failed ('+response.status+')'); }
    if (!response.ok)
    {
        const error=new Error(data.error || 'Request failed ('+response.status+')');
        error.field=data.field;
        throw error;
    }
    return data;
}

// Surface background failures without discarding the last successful dashboard readings.
function notice(message)
{
    const box=document.getElementById('notice');
    box.textContent=message;
    box.hidden=!message;
}

// Render tabular evidence below charts so precise values remain accessible.
function table(id,headers,rows)
{
    const container=document.getElementById(id);
    container.tabIndex=0;
    container.setAttribute('role','region');
    container.setAttribute('aria-label',id.replaceAll('-',' ')+' table');
    setMarkup(container,'<table><thead><tr>'+headers.map(value=>'<th scope="col">'+escapeHtml(value)+'</th>').join('')+'</tr></thead><tbody>'+
        (rows.length ? rows.map(row=>'<tr>'+row.map(value=>'<td>'+value+'</td>').join('')+'</tr>').join('') : '<tr><td colspan="'+headers.length+'">No records in this selection.</td></tr>')+'</tbody></table>');
}

// Display every reported quota bucket; missing windows are never represented as unused quota.
function quotaMarkup(limits)
{
    const buckets=limits?.rateLimitsByLimitId || (limits?.rateLimits ? {codex:limits.rateLimits} : {});
    return Object.entries(buckets).map(([name,bucket])=>'<div><strong>'+escapeHtml(name)+'</strong>'+['primary','secondary'].map(key=>
    {
        const window=bucket[key];
        if (!window || typeof window.usedPercent!=='number') return '';
        let label=key+' window';
        if (window.windowDurationMins>=1440) label=count(window.windowDurationMins/1440)+' days';
        else if (window.windowDurationMins>=60) label=count(window.windowDurationMins/60)+' hours';
        else if (window.windowDurationMins) label=count(window.windowDurationMins)+' minutes';
        return '<div class="quota'+(window.usedPercent>=90?' high':'')+'"><span>'+escapeHtml(label)+'<b>'+count(window.usedPercent)+'% used</b></span><progress aria-label="'+escapeHtml(name+' '+label+' used')+'" max="100" value="'+Math.min(100,Math.max(0,window.usedPercent))+'"></progress><small>Resets '+utc(window.resetsAt)+'</small></div>';
    }).join('')+'</div>').join('') || '<p>No quota reported yet.</p>';
}

// Update status only, preserving a typed test prompt and its reply during automatic refreshes.
async function loadAccounts()
{
    const data=await api('accounts');
    state.accounts=data.accounts;
    const container=document.getElementById('account-cards');
    const homes=new Set(data.accounts.map(account=>account.home));
    for (const card of [...container.children]) if (!homes.has(card.dataset.home)) card.remove();
    if (!data.accounts.length) container.innerHTML='<article class="panel">No AI accounts configured'+(state.report?.development ? ' (development mode)' : '')+'.</article>';
    for (const account of data.accounts)
    {
        let card=[...container.children].find(card=>card.dataset.home===account.home);
        if (!card)
        {
            card=document.createElement('article');
            card.className='account';
            card.dataset.home=account.home;
            card.innerHTML='<div class="account-status"></div><details><summary>Test this account</summary><form><label>Short test prompt<input name="prompt" value="Reply with exactly: OK" maxlength="2000" required></label><button>Run test</button></form><div class="test-result" role="status"></div></details>';
            container.append(card);
        }
        card.querySelector('form').dataset.test=account.id;
        const stale=!account.checked_at || data.now-account.checked_at>330;
        let status=account.limits?.authType==='apiKey'?'Key configured':'Connected';
        if (stale) status='Waiting for refresh';
        if (account.active_requests) status=count(account.active_requests)+' active';
        if (account.error) status='Refresh failed';
        if (account.paused) status='Paused';
        const apiKey=account.limits?.authType==='apiKey';
        const button=card.querySelector('form button'); button.disabled=account.paused || button.dataset.running==='true';
        let badge='';
        if (account.paused || stale) badge='warn';
        if (account.error) badge='error';
        const title=account.email || (apiKey?'OpenAI API '+account.id.slice(0,8):account.home.split(/[\\/]/u).at(-1));
        let quota=quotaMarkup(account.limits);
        if (apiKey)
        {
            const usage=account.usage;
            quota='<p class="hint">Prepaid balance is unavailable through an API key. <a href="https://platform.openai.com/settings/organization/billing" target="_blank" rel="noopener noreferrer">OpenAI billing</a></p>';
            if (usage.attempts)
            {
                const cost=usage.measured?((usage.input-usage.cached)*data.pricing.input+usage.cached*data.pricing.cached+usage.output*data.pricing.output)/1000000:null;
                quota+='<small>Recorded here, all time · input '+count(usage.input)+' · cached '+count(usage.cached)+' · output '+count(usage.output)+'</small><small>Estimated token cost '+money(cost)+' at current configured rates · '+count(usage.measured)+' of '+count(usage.attempts)+' attempts measured</small>';
            }
            else quota+='<small>No API usage recorded here yet.</small>';
        }
        const markup='<span class="badge '+badge+'">'+escapeHtml(status)+'</span><h3>'+escapeHtml(title)+'</h3><p>'+escapeHtml(apiKey?'OpenAI API':account.plan || 'Plan unknown')+'</p>'+quota+'<small>'+count(account.active_requests)+' active turns</small><small>Last checked '+utc(account.checked_at)+'</small><small class="account-home">'+escapeHtml(account.home)+'</small>'+(account.error?'<p>'+escapeHtml(account.error)+'</p>':'')+'<div class="account-actions"><button class="quiet" data-pause="'+escapeHtml(account.id)+'" data-paused="'+account.paused+'">'+(account.paused?'Resume account':'Pause account')+'</button><button class="quiet danger" data-delete="'+escapeHtml(account.id)+'"'+(account.active_requests?' disabled title="Pause the account and wait for its active turns to finish"':'')+'><svg aria-hidden="true"><use href="#icon-remove"/></svg>'+(apiKey?'Delete API key':'Delete account')+'</button></div>';
        const statusBox=card.querySelector('.account-status');
        setMarkup(statusBox,markup);
    }
    state.login=data.login;
    document.getElementById('account-login').hidden=!data.login;
    if (data.login)
    {
        const login=data.login, link=document.getElementById('account-login-link'), code=document.getElementById('account-login-code');
        link.hidden=code.hidden=login.finished;
        link.href=login.verification_url;
        code.textContent=login.user_code;
        document.getElementById('account-login-status').textContent=login.finished?(login.error || 'Account connected.'):'Open OpenAI sign-in and enter this code. Expires '+utc(login.expires_at)+'. This page updates automatically.';
        document.getElementById('account-login-cancel').textContent=login.finished?'Dismiss':'Cancel sign-in';
    }
    const pendingLogin=!!data.login && !data.login.finished;
    document.getElementById('account-connect').disabled=addingAccount || pendingLogin;
    document.querySelector('#api-key-form button').disabled=addingAccount || pendingLogin;
    const select=document.getElementById('account-filter');
    const selected=select.value;
    const options='<option value="">All accounts</option>'+data.accounts.map(account=>'<option value="'+escapeHtml(account.home)+'">'+escapeHtml(account.email || account.home)+'</option>').join('');
    if (setMarkup(select,options)) select.value=selected;
}

// Draw a small dependency-free chart, retaining gaps for unknown measurements.
function chart(id,points,color)
{
    const canvas=document.getElementById(id);
    const width=canvas.clientWidth,height=230,ratio=devicePixelRatio || 1;
    if (!width) return;
    canvas.width=width*ratio;
    canvas.height=height*ratio;
    const ctx=canvas.getContext('2d');
    ctx.scale(ratio,ratio);
    ctx.font='13px system-ui';
    ctx.fillStyle='#61777b';
    canvas.setAttribute('aria-label',points.length?points.map(point=>point.time+': '+count(point.value)).join('; '):'No records in this period.');
    if (!points.length) { ctx.fillText('No records in this period.',16,105); return; }
    const values=points.filter(point=>point.value!==null).map(point=>point.value);
    const high=Math.max(1,...values),low=Math.min(0,...values);
    const times=points.map(point=>Date.parse(point.time.length===10 ? point.time+'T00:00:00Z' : point.time.replace(' ','T')+':00Z'));
    const span=times.at(-1)-times[0];
    const x=index=>58+(width-78)*(span===0 ? .5 : (times[index]-times[0])/span);
    const y=value=>180-150*(value-low)/(high-low);
    for (let i=0;i<4;i++)
    {
        const value=low+(high-low)*i/3;
        ctx.strokeStyle='#e3ebea';
        ctx.beginPath();ctx.moveTo(55,y(value));ctx.lineTo(width-12,y(value));ctx.stroke();
        ctx.fillText(new Intl.NumberFormat(undefined,{notation:'compact',maximumFractionDigits:1}).format(value),2,y(value)+4);
    }
    ctx.strokeStyle=color;ctx.lineWidth=2;ctx.beginPath();
    let pen=false;
    points.forEach((point,index)=>
    {
        if (point.value===null) { pen=false; return; }
        if (pen) ctx.lineTo(x(index),y(point.value)); else ctx.moveTo(x(index),y(point.value));
        pen=true;
    });
    ctx.stroke();ctx.fillStyle=color;
    points.forEach((point,index)=> { if (point.value!==null) { ctx.beginPath();ctx.arc(x(index),y(point.value),3,0,Math.PI*2);ctx.fill(); } });
    ctx.fillStyle='#61777b';ctx.fillText(points[0].time,55,215);
    if (points.length>1) { ctx.textAlign='right';ctx.fillText(points.at(-1).time,width-12,215); }
}

// Build the active report query without sending empty filters as invalid numeric values.
function query()
{
    const result=new URLSearchParams({from:Math.floor(new Date(document.getElementById('from').value).getTime()/1000),to:Math.floor(new Date(document.getElementById('to').value).getTime()/1000)});
    for (const [name,id] of [['user','user-filter'],['account','account-filter'],['purpose','purpose']])
        if (document.getElementById(id).value) result.set(name,document.getElementById(id).value);
    return result;
}

// Render credit and token totals separately; neither is converted into estimated subscription money.
async function loadReport()
{
    const version=++state.reportVersion, filters=new URLSearchParams(state.filters);
    filters.set('page',state.page);
    let data;
    try { data=await api('report?'+filters); } catch (error) { if (version===state.reportVersion) throw error; return; }
    if (version!==state.reportVersion) return;
    state.report=data;
    const summary=data.summary,credits=data.credits;
    document.getElementById('coverage').textContent=data.coverage.since?'Token recording began '+utc(data.coverage.since)+'. Earlier token consumption is unknown.':'No provider usage recorded yet.';
    document.getElementById('applied-filters').textContent='Showing '+new Date(Number(filters.get('from'))*1000).toLocaleString()+' – '+new Date(Number(filters.get('to'))*1000).toLocaleString()+' · '+(filters.has('user')?'User '+filters.get('user'):'All users')+' · '+(filters.get('account') || 'All accounts')+' · '+(filters.get('purpose') || 'All purposes');
    const metrics=[['Measured tokens',summary.total_tokens,count(summary.measured)+' of '+count(summary.attempts)+' attempts measured'],['Provider attempts',summary.attempts,count(summary.running || 0)+' running · '+count(summary.failed || 0)+' failed'],['AI credits used',credits.debited-credits.refunded,'All accounts and purposes · '+count(credits.refunded)+' refunded'],['Credits added',credits.added,'Grants, payments and positive adjustments']];
    document.getElementById('metrics').innerHTML=metrics.map(([title,value,note])=>'<article class="metric"><span>'+title+'</span><strong>'+count(value)+'</strong><small>'+escapeHtml(note)+'</small></article>').join('');
    chart('token-chart',data.timeline.map(row=>({time:row.time,value:row.total_tokens})),'#087e74');
    chart('credit-chart',data.credit_timeline.map(row=>({time:row.time,value:row.debited-row.refunded})),'#6178ac');
    document.getElementById('current-pricing').textContent='Current USD rates per million tokens: input '+money(data.pricing.input)+', cached input '+money(data.pricing.cached)+', output '+money(data.pricing.output)+'. Costs below reprice recorded tokens at these rates; they are not historical charges. Cached input is excluded from input cost. Missing usage is not estimated; mixed rows show measured usage only.';
    for (const [id,rows] of [['account-breakdown',data.accounts],['purpose-breakdown',data.purposes],['model-breakdown',data.models]])
        table(id,['Source','Attempts','Credits charged','Input','Cached input','Output','Reasoning','Total tokens','Input USD','Cached USD','Output USD','Total USD'],rows.map(row=>
        {
            const input=row.input_tokens===null || row.cached_tokens===null?null:(row.input_tokens-row.cached_tokens)*data.pricing.input/1000000;
            const cached=row.cached_tokens===null?null:row.cached_tokens*data.pricing.cached/1000000;
            const output=row.output_tokens===null?null:row.output_tokens*data.pricing.output/1000000;
            const total=[input,cached,output].includes(null)?null:input+cached+output;
            return [escapeHtml(row.name || 'Not reported'),count(row.attempts),count(row.credits_charged),count(row.input_tokens),count(row.cached_tokens),count(row.output_tokens),count(row.reasoning_tokens),count(row.total_tokens),...[input,cached,output,total].map(value=>escapeHtml(money(value)))];
        }));
    table('activity',['Started (UTC)','User / purpose','Account / model','Result','Credits','Tokens','Input / cached / output / reasoning','Request / conversation'],data.activity.map(row=>[
        utc(row.started_at),escapeHtml(row.email || 'Operator')+'<br>'+escapeHtml(row.purpose),escapeHtml(row.account_home)+'<br>'+escapeHtml(row.model || 'Not reported'),
        '<span class="badge '+(row.error ? 'error' : '')+'">'+(row.error ? 'Failed' : row.finished_at ? 'Completed' : 'Running')+'</span><br>'+escapeHtml(row.error || ''),count(row.credits_charged),count(row.total_tokens),[row.input_tokens,row.cached_tokens,row.output_tokens,row.reasoning_tokens].map(count).join(' / '),
        '<details><summary>Identifiers</summary><p>Request<br><code>'+escapeHtml(row.request_id)+'</code></p><p>Conversation<br><code>'+escapeHtml(row.conversation_id)+'</code></p></details>'
    ]));
    table('ledger',['Time (UTC)','User','Credit change','Reference','Reason'],data.ledger.map(row=>[utc(row.created_at),escapeHtml(row.email),count(row.delta),escapeHtml(row.reference),escapeHtml(row.note || '')]));
    table('quota-history',['Time (UTC)','Account','Primary used','Secondary used'],data.quotas.map(row=>
    {
        const limits=JSON.parse(row.limits);
        const bucket=limits.rateLimitsByLimitId?.codex || limits.rateLimits;
        return [utc(row.checked_at),escapeHtml(row.account_home),bucket?.primary ? count(bucket.primary.usedPercent)+'%' : 'Unknown',bucket?.secondary ? count(bucket.secondary.usedPercent)+'%' : 'Unknown'];
    }));
    document.getElementById('page-number').textContent='Both tables · Page '+(state.page+1);
    document.getElementById('previous').disabled=state.page===0;
    document.getElementById('next').disabled=data.activity.length<100 && data.ledger.length<100;
    document.getElementById('payment-status').textContent=data.payments.stripe ? 'Stripe configured · '+data.payments.packs.length+' credit packs' : 'Stripe is not enabled'+(data.development ? ' in development mode.' : '. Configure it on the server to create payment links.');
}

// Refresh the user list independently of report filters and preserve open operator forms.
async function loadUsers()
{
    const version=++state.userVersion;
    let data;
    try { data=await api('users?'+new URLSearchParams({page:state.userPage,search:state.search})); } catch (error) { if (version===state.userVersion) throw error; return; }
    if (version!==state.userVersion) return;
    state.users=data.users;
    document.getElementById('user-count').textContent=count(data.total);
    table('user-table',['ID','Email','Access','Credits','Measured tokens','Coverage gaps','Requests','Manage'],data.users.map(user=>
    {
        let access='Active',style='';
        if (user.credits<=0) { access='No credits'; style='warn'; }
        if (user.blocked) { access='Blocked'; style='error'; }
        const gaps=user.unknown_attempts+user.unmeasured_requests;
        return [user.id,escapeHtml(user.email),'<span class="badge '+style+'">'+access+'</span>',count(user.credits),count(user.tokens),
            gaps?'<details><summary>'+count(gaps)+(gaps===1?' gap':' gaps')+'</summary><p>'+count(user.unknown_attempts)+' unknown attempts<br>'+count(user.unmeasured_requests)+' unmeasured requests</p></details>':'None',count(user.requests),'<button class="quiet" data-user="'+user.id+'">Manage</button>'];
    }));
    document.getElementById('user-page').textContent='Page '+(state.userPage+1)+' · '+count(data.matching)+' matching users';
    document.getElementById('user-previous').disabled=state.userPage===0;
    document.getElementById('user-next').disabled=(state.userPage+1)*50>=data.matching;
}

// Cursor pages remain stable during auto-refresh; expanded diagnostics keep their DOM nodes.
async function loadLogs()
{
    const version=++logs.version, query=new URLSearchParams({search:logs.search});
    if (logs.before) query.set('before',logs.before);
    if (logs.level) query.set('level',logs.level);
    const data=await api('logs?'+query);
    if (version!==logs.version) return;
    logs.entries=data.entries; logs.next=data.next_before;
    const container=document.getElementById('log-entries'), present=new Set();
    data.entries.forEach((entry,index)=>
    {
        const id=String(entry.id); present.add(id);
        let row=[...container.children].find(row=>row.dataset.id===id);
        if (!row)
        {
            row=document.createElement('details'); row.className='diagnostic panel'; row.dataset.id=id;
            row.innerHTML='<summary><svg class="log-chevron" aria-hidden="true"><use href="#icon-chevron"/></svg><span class="badge '+({error:'error',warning:'warn'}[entry.level] || '')+'">'+escapeHtml(entry.level)+'</span><time>'+escapeHtml(utc(entry.created_at))+'</time><strong>'+escapeHtml(entry.stage)+'</strong><span class="diagnostic-preview">'+escapeHtml(entry.message.split('\n')[0])+'</span></summary><p>'+escapeHtml([entry.request_id?'Request '+entry.request_id:'',entry.account?'Account '+entry.account:''].filter(Boolean).join(' · '))+'</p><pre></pre>';
            row.querySelector('pre').textContent=entry.message;
        }
        if (container.children[index]!==row) container.insertBefore(row,container.children[index] || null);
    });
    for (const row of [...container.children]) if (!present.has(row.dataset.id)) row.remove();
    document.getElementById('older-logs').disabled=!logs.next;
    document.getElementById('download-logs').disabled=!logs.entries.length;
    document.getElementById('log-page').textContent=logs.entries.length?logs.entries.length+' entries · '+(logs.before?'Older page':'Latest'):'No matching diagnostics';
}

// Refresh cached backend data without initiating extra ChatGPT quota requests.
async function refresh()
{
    if (state.refreshing) return;
    state.refreshing=true;
    try
    {
        const results=await Promise.allSettled([loadAccounts(),loadReport(),loadUsers(),loadLogs()]);
        const failed=results.find(result=>result.status==='rejected');
        if (failed) throw failed.reason;
        document.getElementById('connection').textContent='Display updated '+new Date().toLocaleTimeString();
        notice('');
    }
    catch (error) { notice(error.message); document.getElementById('connection').textContent='Refresh failed · showing last available data'; }
    finally { state.refreshing=false; }
}

// One pending sign-in is shared by all admin tabs; credentials are never retained in UI state.
async function addAccount(key)
{
    if (addingAccount) return;
    addingAccount=true;
    document.getElementById('account-connect').disabled=true;
    document.querySelector('#api-key-form button').disabled=true;
    const status=document.getElementById('account-add-status'); status.textContent='Connecting…';
    try
    {
        await api('accounts',{api_key:key});
        document.getElementById('api-key').value='';
        status.textContent=key?'API key saved. Run a short test to verify model access.':'Complete sign-in using the code below.';
    }
    catch (error) { status.textContent=error.message; }
    finally { addingAccount=false; await loadAccounts().catch(error=>notice(error.message)); }
}
document.getElementById('account-connect').addEventListener('click',()=>addAccount(null));
document.getElementById('api-key-form').addEventListener('submit',event=>{event.preventDefault();addAccount(document.getElementById('api-key').value.trim());});
document.getElementById('account-login-cancel').addEventListener('click',async event=>
{
    const button=event.currentTarget; button.disabled=true;
    try { await api('accounts/login/'+state.login.id+'/cancel',{}); await loadAccounts(); }
    catch (error) { notice(error.message); }
    finally { button.disabled=false; }
});
document.getElementById('account-cards').addEventListener('click',async event=>
{
    const button=event.target.closest('[data-pause],[data-delete]');
    if (!button) return;
    const deleting=!!button.dataset.delete,id=button.dataset.delete || button.dataset.pause;
    const account=state.accounts.find(account=>account.id===id);
    if (deleting && !confirm('Delete saved credentials for '+(account.email || 'this '+(account.limits?.authType==='apiKey'?'API key':'ChatGPT account'))+' from this backend? Recorded usage history will be kept.')) return;
    button.disabled=true;
    try
    {
        await api('accounts/'+id+(deleting?'/delete':'/pause'),deleting?{}:{paused:button.dataset.paused!=='true'});
        await loadAccounts();
    }
    catch (error) { notice(error.message); }
    finally { button.disabled=false; }
});

document.getElementById('account-cards').addEventListener('submit',async event=>
{
    event.preventDefault();
    const form=event.target,button=form.querySelector('button'),result=form.parentElement.querySelector('.test-result');
    button.disabled=true;button.dataset.running='true';result.textContent='Waiting for this account…';
    try { result.textContent=(await api('accounts/'+form.dataset.test+'/test',{prompt:form.elements.prompt.value})).text; await loadReport(); }
    catch (error) { result.textContent=error.message; }
    finally { delete button.dataset.running; button.disabled=!!state.accounts.find(account=>account.id===form.dataset.test)?.paused; }
});
document.getElementById('filters').addEventListener('submit',event=> { event.preventDefault();state.page=0;state.filters=query();loadReport().catch(error=>notice(error.message)); });
document.getElementById('reset-filters').addEventListener('click',()=> { resetFilters();loadReport().catch(error=>notice(error.message)); });
document.getElementById('user-search').addEventListener('submit',event=> { event.preventDefault();state.userPage=0;state.search=document.getElementById('search').value.trim();loadUsers().catch(error=>notice(error.message)); });
for (const [id,key,step,load] of [['previous','page',-1,loadReport],['next','page',1,loadReport],['user-previous','userPage',-1,loadUsers],['user-next','userPage',1,loadUsers]])
    document.getElementById(id).addEventListener('click',()=> { state[key]=Math.max(0,state[key]+step);load().catch(error=>notice(error.message)); });
document.getElementById('refresh').addEventListener('click',refresh);
document.getElementById('log-filters').addEventListener('submit',event=>
{
    event.preventDefault(); logs.before=null; logs.search=document.getElementById('log-search').value.trim(); logs.level=document.getElementById('log-level').value;
    loadLogs().catch(error=>notice(error.message));
});
document.getElementById('latest-logs').addEventListener('click',()=>{logs.before=null; loadLogs().catch(error=>notice(error.message));});
document.getElementById('older-logs').addEventListener('click',()=>{logs.before=logs.next; loadLogs().catch(error=>notice(error.message));});
document.getElementById('download-logs').addEventListener('click',()=>
{
    const text=logs.entries.map(entry=>JSON.stringify(entry)).join('\n')+'\n';
    const url=URL.createObjectURL(new Blob([text],{type:'application/x-ndjson;charset=utf-8'})),link=document.createElement('a');
    link.href=url; link.download='assistant-diagnostics.jsonl'; link.click(); setTimeout(()=>URL.revokeObjectURL(url),1000);
});
document.getElementById('logout').addEventListener('click',async()=> { try { await api('logout',{});location.href='/admin/login'; } catch (error) { notice(error.message); } });
document.getElementById('close-user').addEventListener('click',()=>document.getElementById('user-dialog').close());
document.getElementById('user-table').addEventListener('click',event=>
{
    const button=event.target.closest('[data-user]');
    if (!button) return;
    state.selected=state.users.find(user=>user.id===Number(button.dataset.user));
    state.adjustment=null;
    document.getElementById('selected-email').textContent=state.selected.email;
    document.getElementById('selected-user').textContent='User '+state.selected.id+' · '+count(state.selected.credits)+' credits';
    document.getElementById('block-user').textContent=state.selected.blocked ? 'Unblock user' : 'Block user';
    document.getElementById('credit-form').reset();
    document.getElementById('user-result').textContent='';
    document.getElementById('checkout-url').hidden=true;
    document.getElementById('create-checkout').disabled=!state.report?.payments.stripe || !state.report.payments.packs.length;
    document.getElementById('checkout-pack').innerHTML=(state.report?.payments.packs || []).map(pack=>'<option value="'+escapeHtml(pack.price_id)+'">'+count(pack.credits)+' credits — '+money(Number(pack.usd))+'</option>').join('');
    document.getElementById('user-dialog').showModal();
});
document.getElementById('credit-form').addEventListener('submit',async event=>
{
    event.preventDefault();
    const user=state.selected;
    const button=event.target.querySelector('button'),delta=Number(document.getElementById('credit-delta').value),note=document.getElementById('credit-note').value;
    if (!state.adjustment || state.adjustment.delta!==delta || state.adjustment.note!==note) state.adjustment={id:crypto.randomUUID(),delta,note};
    button.disabled=true;
    try
    {
        const result=await api('users/'+user.id+'/credits',state.adjustment);
        if (state.selected!==user) { await refresh(); return; }
        user.credits=result.credits;
        state.adjustment=null;
        event.target.reset();
        document.getElementById('user-result').textContent='Adjustment saved. Balance: '+count(result.credits)+' credits.';
        document.getElementById('selected-user').textContent='User '+state.selected.id+' · '+count(result.credits)+' credits';
        await refresh();
    }
    catch (error) { if (state.selected===user) document.getElementById('user-result').textContent=error.message; }
    finally { button.disabled=false; }
});
document.getElementById('block-user').addEventListener('click',async event=>
{
    const button=event.currentTarget,user=state.selected;button.disabled=true;
    try
    {
        await api('users/'+user.id+'/block',{blocked:!user.blocked});
        user.blocked=!user.blocked;
        if (state.selected===user)
        {
            button.textContent=user.blocked ? 'Unblock user' : 'Block user';
            document.getElementById('user-result').textContent=user.blocked ? 'User blocked. New authenticated requests are denied.' : 'User unblocked.';
        }
        await loadUsers();
    }
    catch (error) { if (state.selected===user) document.getElementById('user-result').textContent=error.message; }
    finally { button.disabled=false; }
});
document.getElementById('revoke-user').addEventListener('click',async event=>
{
    const user=state.selected,button=event.currentTarget;
    button.disabled=true;
    try { await api('users/'+user.id+'/revoke',{});if (state.selected===user) document.getElementById('user-result').textContent='All application sessions revoked.'; }
    catch (error) { if (state.selected===user) document.getElementById('user-result').textContent=error.message; }
    finally { button.disabled=false; }
});
document.getElementById('create-checkout').addEventListener('click',async event=>
{
    const button=event.currentTarget,user=state.selected;button.disabled=true;
    try
    {
        const result=await api('users/'+user.id+'/checkout',{price_id:document.getElementById('checkout-pack').value}),link=document.getElementById('checkout-url');
        if (state.selected!==user) return;
        const url=new URL(result.url);
        if (url.protocol!=='https:' || url.hostname!=='checkout.stripe.com') throw new Error('Unexpected payment URL');
        link.href=url.href;link.textContent=url.href;link.hidden=false;
        document.getElementById('user-result').textContent='Payment link created. Credits are added only after Stripe confirms payment.';
    }
    catch (error) { if (state.selected===user) document.getElementById('user-result').textContent=error.message; }
    finally { button.disabled=false; }
});
document.getElementById('filter-user').addEventListener('click',()=>
{
    document.getElementById('user-filter').value=state.selected.id;state.filters.set('user',state.selected.id);state.page=0;document.getElementById('user-dialog').close();location.hash='usage';loadReport().catch(error=>notice(error.message));
});
window.addEventListener('resize',()=>
{
    if (!state.report) return;
    chart('token-chart',state.report.timeline.map(row=>({time:row.time,value:row.total_tokens})),'#087e74');
    chart('credit-chart',state.report.credit_timeline.map(row=>({time:row.time,value:row.debited-row.refunded})),'#6178ac');
});
// Keep drafts separate from the applied query until the operator explicitly applies or resets filters.
function resetFilters()
{
    document.getElementById('filters').reset();
    document.getElementById('from').value=localInput(new Date(Date.now()-7*86400000));
    document.getElementById('to').value=localInput(new Date(Date.now()+86400000));
    state.page=0;
    state.filters=query();
}
resetFilters();
refresh();
setInterval(()=> { if (!document.hidden) refresh(); },10000);
