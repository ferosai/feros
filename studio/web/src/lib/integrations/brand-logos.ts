export interface BrandLogo {
  alt: string;
  src: string;
}

export const brandLogos = {
  airtable: { alt: "Airtable", src: "/brands/integrations/airtable.svg" },
  brevo: { alt: "Brevo", src: "/brands/integrations/brevo.svg" },
  cal_com: { alt: "Cal.com", src: "/brands/integrations/cal_com.svg" },
  calendly: { alt: "Calendly", src: "/brands/integrations/calendly.svg" },
  clickup: { alt: "ClickUp", src: "/brands/integrations/clickup.svg" },
  discord: { alt: "Discord", src: "/brands/integrations/discord.svg" },
  freshdesk: { alt: "Freshdesk", src: "/brands/integrations/freshdesk.svg" },
  ghost: { alt: "Ghost", src: "/brands/integrations/ghost.svg" },
  google_calendar: {
    alt: "Google Calendar",
    src: "/brands/integrations/google_calendar.svg",
  },
  google_docs: {
    alt: "Google Docs",
    src: "/brands/integrations/google_docs.svg",
  },
  google_sheets: {
    alt: "Google Sheets",
    src: "/brands/integrations/google_sheets.svg",
  },
  gohighlevel: {
    alt: "GoHighLevel",
    src: "/brands/integrations/gohighlevel.svg",
  },
  grafana: { alt: "Grafana", src: "/brands/integrations/grafana.svg" },
  hubspot: { alt: "HubSpot", src: "/brands/integrations/hubspot.svg" },
  jotform: { alt: "Jotform", src: "/brands/integrations/jotform.svg" },
  lemlist: { alt: "lemlist", src: "/brands/integrations/lemlist.svg" },
  mailgun: { alt: "Mailgun", src: "/brands/integrations/mailgun.svg" },
  mailjet: { alt: "Mailjet", src: "/brands/integrations/mailjet.svg" },
  metabase: { alt: "Metabase", src: "/brands/integrations/metabase.svg" },
  microsoft_outlook: {
    alt: "Microsoft Outlook",
    src: "/brands/integrations/microsoft_outlook.svg",
  },
  pagerduty: { alt: "PagerDuty", src: "/brands/integrations/pagerduty.svg" },
  salesforce: { alt: "Salesforce", src: "/brands/integrations/salesforce.svg" },
  salesforce_sandbox: {
    alt: "Salesforce Sandbox",
    src: "/brands/integrations/salesforce_sandbox.svg",
  },
  sendgrid: { alt: "SendGrid", src: "/brands/integrations/sendgrid.svg" },
  slack: { alt: "Slack", src: "/brands/integrations/slack.svg" },
  supabase: { alt: "Supabase", src: "/brands/integrations/supabase.svg" },
  twilio: { alt: "Twilio", src: "/brands/integrations/twilio.svg" },
  typeform: { alt: "Typeform", src: "/brands/integrations/typeform.svg" },
} satisfies Record<string, BrandLogo>;
