'use strict';
const questionViews=new Map();

// Keep consecutive native tool activity in one disclosure; commentary, plans and user input break the group.
function renderProgress(container,parts,active=false,steers=[])
{
    let rows=progressViews.get(container);
    if (!rows) { rows=new Map(); progressViews.set(container,rows); }
    const groups=[];
    for (let index=0;index<=parts.length;index++)
    {
        for (const input of steers.filter(input=>Math.min(input.progress_after,parts.length)===index)) groups.push({key:'steer:'+input.id,kind:'steer',input});
        const part=parts[index];
        if (!part?.text.trim()) continue;
        const work=['reasoning','action','web_search'].includes(part.kind), previous=groups.at(-1);
        if (work && previous?.kind==='work') previous.parts.push(part);
        else groups.push({key:part.id,kind:work?'work':part.kind,parts:[part]});
    }
    const present=new Set();
    groups.forEach((group,index)=>
    {
        let row=rows.get(group.key);
        if (!row)
        {
            const element=group.kind==='steer'?steeringRows.get(group.input.id):document.createElement(group.kind==='work'?'details':'div');
            let label,body=element;
            if (group.kind==='work')
            {
                element.className='tool-result';
                const summary=document.createElement('summary');
                summary.innerHTML='<svg aria-hidden="true"><use href="#icon-terminal"/></svg><span></span><svg class="disclosure-arrow" aria-hidden="true"><use href="#icon-chevron"/></svg>';
                label=summary.querySelector('span'); body=document.createElement('div'); element.append(summary,body);
            }
            else if (group.kind==='plan') element.className='live-plan';
            else if (group.kind!=='steer') element.className='answer progress-commentary';
            row={element,label,body,actions:new Map(),reasoning:new Map()}; rows.set(group.key,row);
        }
        if (group.kind==='work') renderWork(row,group.parts,active && index===groups.length-1);
        else if (group.kind==='plan')
        {
            const part=group.parts[0], key=JSON.stringify(part.data);
            if (row.key!==key)
            {
                row.key=key; row.body.replaceChildren();
                const heading=document.createElement('strong'); heading.textContent='Plan'; row.body.append(heading);
                if (part.data.explanation) { const text=document.createElement('p'); text.textContent=part.data.explanation; row.body.append(text); }
                const list=document.createElement('ol');
                for (const step of part.data.plan)
                {
                    const item=document.createElement('li'), icon=document.createElement('span'), text=document.createElement('span');
                    item.dataset.status=step.status; icon.className='plan-state';
                    if (step.status==='completed') setIcon(icon,'check');
                    else icon.textContent=String(list.children.length+1);
                    text.textContent=step.step; item.append(icon,text); list.append(item);
                }
                row.body.append(list);
            }
        }
        else if (group.kind==='commentary') renderAnswer(row.body,group.parts.map(part=>part.text).join('\n\n'));
        present.add(group.key);
        if (container.children[index]!==row.element) container.insertBefore(row.element,container.children[index] || null);
    });
    for (const [key,row] of rows) if (!present.has(key)) { if (row.element.parentElement===container) row.element.remove(); rows.delete(key); }
}

// Polling reuses its session row, while verified changes and reasoning retain their own nested disclosures.
function renderWork(row,parts,active)
{
    const actions=parts.filter(part=>part.kind!=='reasoning'), changes=actions.flatMap(part=>view.tool_results[part.id]?.changes || []);
    let label=actions.at(-1)?.text || 'Thinking…';
    if (!active && actions.length)
    {
        const commands=actions.some(part=>['local_exec_command','local_write_stdin','local_stop_process'].includes(part.data?.tool));
        const files=actions.some(part=>['local_read_file','local_list_directory','local_search_files','local_get_metadata'].includes(part.data?.tool));
        const images=actions.some(part=>part.data?.tool==='local_view_image');
        const modified=actions.some(part=>['local_create_directory','local_copy_path','local_move_path','local_remove_path','local_create_symlink','local_set_permissions'].includes(part.data?.tool) && view.tool_results[part.id]?.success);
        label=[changes.length?'Edited files':modified?'Updated files':files?'Read files':'',images?'viewed images':'',commands?'ran commands':'',actions.some(part=>part.kind==='web_search')?'searched the web':''].filter(Boolean).join(', ');
        label=label?label[0].toUpperCase()+label.slice(1):'Completed actions';
    }
    const title=progressTitle(parts) || label;
    if (row.label.textContent!==title) row.label.textContent=title;
    row.label.classList.toggle('working',active);
    const icon=row.element.querySelector('summary>svg:first-child'); icon.toggleAttribute('hidden',!actions.length);
    const hasCommands=actions.some(part=>['local_exec_command','local_write_stdin','local_stop_process'].includes(part.data?.tool));
    let groupIcon='list-ul';
    if (changes.length) groupIcon='edit';
    else if (hasCommands) groupIcon='terminal';
    else if (actions.some(part=>part.kind==='web_search')) groupIcon='external';
    const use=icon.querySelector('use');
    if (use.getAttribute('href')!=='#icon-'+groupIcon) use.setAttribute('href','#icon-'+groupIcon);
    const sessions=new Map(), present=new Set(), reasoning=new Set();
    for (const part of actions)
    {
        const result=view.tool_results[part.id], session=result?.session_id || part.data?.arguments?.session_id || part.id;
        if (!sessions.has(session)) sessions.set(session,{first:part.id,parts:[]});
        sessions.get(session).parts.push(part);
    }
    row.actionPosition=0;
    for (const part of parts)
    {
        if (part.kind==='reasoning')
        {
            let element=row.reasoning.get(part.id);
            if (!element)
            {
                element=document.createElement('details'); element.className='past-progress';
                element.innerHTML='<summary><span></span><svg aria-hidden="true"><use href="#icon-chevron"/></svg></summary><div class="answer progress-body"></div>';
                row.reasoning.set(part.id,element);
            }
            const title=progressTitle([part]) || 'Thinking', label=element.querySelector('summary span');
            if (label.textContent!==title) label.textContent=title;
            renderAnswer(element.lastChild,part.text);
            const position=row.body.children[row.actionPosition++]; if (position!==element) row.body.insertBefore(element,position || null);
            reasoning.add(part.id); continue;
        }
        const result=view.tool_results[part.id], id=result?.session_id || part.data?.arguments?.session_id || part.id, session=sessions.get(id);
        if (session.first!==part.id) continue;
        const last=session.parts.at(-1), latest=view.tool_results[last.id], args=part.data?.arguments || {};
        const output=session.parts.map(item=>view.tool_results[item.id]?.output || '').filter(Boolean).join('\n\n');
        const command=part.data?.tool==='local_exec_command';
        const running=latest?.running ?? !last.completed;
        const caption=command?(running?'Running ':'Ran ')+args.cmd:part.text;
        const toolIcon=part.kind==='web_search'?'external':command || args.session_id?'terminal':(result?.changes || []).length?'edit':'list-ul';
        // The first native item stays stable when the local session ID arrives later.
        const key=session.first, process=result?.session_id || args.session_id;
        present.add(key); updateAction(row,key,toolIcon,caption,output || JSON.stringify(part.data?.action || args,null,2),process);
        const changes=new Map();
        for (const item of session.parts)
            for (const change of view.tool_results[item.id]?.changes || []) changes.set(change.path,change);
        for (const change of changes.values())
        {
            const key=session.first+':'+change.path;
            present.add(key); updateAction(row,key,'edit',change.kind+' '+change.path,change.diff,null,change);
        }
    }
    for (const [key,item] of row.actions) if (!present.has(key)) { item.element.remove(); row.actions.delete(key); }
    for (const [key,element] of row.reasoning) if (!reasoning.has(key)) { element.remove(); row.reasoning.delete(key); }
}

// Reuse pending forms so streaming updates and browser reconnects never clear an answer being typed.
function renderQuestions()
{
    const requests=view.interactions, present=new Set(requests.map(request=>request.id));
    for (const [id,form] of questionViews) if (!present.has(id)) { form.remove(); questionViews.delete(id); }
    for (const request of requests)
    {
        if (questionViews.has(request.id)) continue;
        const form=document.createElement('form'); form.className='native-request';
        const heading=document.createElement('h2'); heading.textContent=request.type==='questions'?'Your input is needed':'Allow access on your computer?'; form.append(heading);
        if (request.type==='questions')
        {
            for (const [questionIndex,question] of request.questions.entries())
            {
                const field=document.createElement('fieldset'), legend=document.createElement('legend'); legend.textContent=question.question; field.append(legend);
                for (const [index,option] of (question.options || []).entries())
                {
                    const label=document.createElement('label'), radio=document.createElement('input'), text=document.createElement('span'), title=document.createElement('strong'), description=document.createElement('small');
                    radio.type='radio'; radio.name='q'+questionIndex; radio.value=String(index); radio.required=true;
                    title.textContent=option.label; description.textContent=option.description; text.append(title,description); label.append(radio,text); field.append(label);
                }
                if (question.isOther || !question.options?.length)
                {
                    const label=document.createElement('label'), radio=document.createElement('input'), input=document.createElement('textarea');
                    radio.type='radio'; radio.name='q'+questionIndex; radio.value='custom'; radio.setAttribute('aria-label','Custom answer');
                    input.name='custom'+questionIndex; input.placeholder='Your own answer'; input.rows=2; input.maxLength=8000;
                    if (question.isSecret) { const secret=document.createElement('input'); secret.type='password'; secret.name=input.name; secret.placeholder=input.placeholder; secret.maxLength=8000; label.append(radio,secret); secret.addEventListener('input',()=>{ radio.checked=true; }); }
                    else { label.append(radio,input); input.addEventListener('input',()=>{ radio.checked=true; }); }
                    field.append(label);
                }
                form.append(field);
            }
        }
        else
        {
            const reason=document.createElement('p'), details=document.createElement('details'), summary=document.createElement('summary'), pre=document.createElement('pre');
            reason.textContent=request.action.reason; summary.textContent='Operation and exact arguments'; pre.textContent=JSON.stringify(request.action,null,2);
            details.append(summary,pre); form.append(reason,details);
        }
        const buttons=document.createElement('div'), submit=document.createElement('button'), status=document.createElement('p');
        buttons.className='button-row'; submit.className='primary'; submit.textContent=request.type==='questions'?'Continue':'Allow'; buttons.append(submit);
        if (request.type==='tool')
        {
            const decline=document.createElement('button'); decline.type='submit'; decline.name='decline'; decline.textContent='Decline'; buttons.append(decline);
        }
        form.append(buttons,status);
        form.addEventListener('submit',async event=>
        {
            event.preventDefault(); const data=new FormData(form); let answer;
            if (request.type==='tool') answer={type:'approval',approve:event.submitter?.name!=='decline'};
            else
            {
                const answers=Object.create(null);
                for (const [index,question] of request.questions.entries())
                {
                    const value=data.get('q'+index);
                    if (value===null) { status.textContent='Please answer every question'; return; }
                    const text=value==='custom'?String(data.get('custom'+index) || '').trim():question.options[Number(value)].label;
                    if (!text) { status.textContent='Please enter your answer'; return; }
                    answers[question.id]=[text];
                }
                answer={type:'questions',answers};
            }
            for (const input of form.elements) input.disabled=true;
            try { await api('respond',{id:request.id,answer}); }
            catch (error) { status.textContent=error.message; for (const input of form.elements) input.disabled=false; }
        });
        questionViews.set(request.id,form); elements.interactions.append(form);
    }
    elements.interactions.hidden=!requests.length;
}
