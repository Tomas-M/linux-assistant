'use strict';
const receiptElements=Object.fromEntries([...document.querySelectorAll('[id]')].map(element=>[element.id,element]));
receiptElements.close.addEventListener('click',()=>window.close());

// Confirmation comes only from the backend's settled purchase, never from the redirect itself.
async function confirmPayment()
{
    if (location.pathname.endsWith('/cancel'))
    {
        receiptElements.spinner.hidden=true;
        receiptElements.eyebrow.textContent='Checkout cancelled';
        receiptElements.heading.textContent='You can return to your assistant';
        receiptElements.description.textContent='You can choose a credit pack and try again whenever you’re ready.';
        receiptElements['close-note'].textContent='Close this window to continue.';
        document.title='Checkout cancelled';
        return;
    }
    const id=new URLSearchParams(location.search).get('purchase_id');
    if (!/^[a-f0-9]{64}$/u.test(id || ''))
    {
        receiptElements.spinner.hidden=true;
        receiptElements.heading.textContent='Payment reference unavailable';
        receiptElements.description.textContent='Return to your assistant to check your balance.';
        receiptElements['close-note'].textContent='You can close this window.';
        return;
    }
    for (let attempt=0;attempt<120;attempt++)
    {
        try
        {
            const response=await fetch('/payment/status/'+id,{cache:'no-store',signal:AbortSignal.timeout(10000)});
            const payment=await response.json();
            if (!response.ok) throw new Error('Payment confirmation unavailable');
            if (payment.paid)
            {
                receiptElements.spinner.hidden=true; receiptElements['success-icon'].removeAttribute('hidden');
                receiptElements.eyebrow.textContent='Payment successful';
                receiptElements.heading.textContent='Thank you for your payment';
                receiptElements.description.textContent='Your credits have been added. Your assistant will update the balance automatically.';
                receiptElements.credits.textContent=payment.credits+' credits added'; receiptElements.credits.hidden=false;
                receiptElements['close-note'].textContent='This window will close in a few seconds.';
                document.title='Payment successful';
                setTimeout(()=>{ window.close(); receiptElements['close-note'].textContent='You can close this window and return to your assistant.'; },3000);
                return;
            }
        }
        catch { receiptElements.description.textContent='We’re waiting for payment confirmation. Your credits will appear as soon as it arrives.'; }
        await new Promise(resolve=>setTimeout(resolve,2000));
    }
    receiptElements.spinner.hidden=true;
    receiptElements.heading.textContent='Your payment is still being confirmed';
    receiptElements.description.textContent='Some payment methods take longer. Your credits will appear automatically once the payment is confirmed.';
    receiptElements['close-note'].textContent='You can close this window and return to your assistant.';
}
confirmPayment();
