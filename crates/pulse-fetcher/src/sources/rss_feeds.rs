use super::RawArticle;

const FEEDS: &[(&str, &str, &str, &str)] = &[
    // === AI (16 feeds) ===
    ("ai", "OpenAI Blog", "https://openai.com/blog/rss/", "en"),
    ("ai", "Google AI Blog", "https://ai.googleblog.com/feeds/posts/default", "en"),
    ("ai", "DeepMind Blog", "https://deepmind.com/blog/feed/basic/", "en"),
    ("ai", "HuggingFace Blog", "https://huggingface.co/blog/feed.xml", "en"),
    ("ai", "ArXiv AI", "https://rss.arxiv.org/rss/cs.AI", "en"),
    ("ai", "VentureBeat AI", "https://venturebeat.com/category/ai/feed/", "en"),
    ("ai", "MIT Tech Review AI", "https://www.technologyreview.com/topic/artificial-intelligence/feed", "en"),
    ("ai", "The Batch (deeplearning.ai)", "https://www.deeplearning.ai/the-batch/feed/", "en"),
    ("ai", "Anthropic Blog", "https://www.anthropic.com/feed.xml", "en"),
    ("ai", "Meta AI Blog", "https://ai.meta.com/blog/rss/", "en"),
    ("ai", "NVIDIA AI Blog", "https://blogs.nvidia.com/feed/", "en"),
    ("ai", "ArXiv Machine Learning", "https://rss.arxiv.org/rss/cs.LG", "en"),
    ("ai", "ArXiv Computation & Language", "https://rss.arxiv.org/rss/cs.CL", "en"),
    ("ai", "The Gradient", "https://thegradient.pub/rss/", "en"),
    ("ai", "Towards Data Science", "https://towardsdatascience.com/feed", "en"),
    ("ai", "Synced AI", "https://syncedreview.com/feed/", "en"),
    ("ai", "AI News", "https://www.artificialintelligence-news.com/feed/", "en"),
    ("ai", "Weights & Biases Blog", "https://wandb.ai/fully-connected/rss.xml", "en"),
    ("ai", "Microsoft AI Blog", "https://blogs.microsoft.com/ai/feed/", "en"),
    ("ai", "Amazon Science", "https://www.amazon.science/index.rss", "en"),
    ("ai", "Apple ML Research", "https://machinelearning.apple.com/rss.xml", "en"),
    ("ai", "AI Alignment Forum", "https://www.alignmentforum.org/feed.xml", "en"),
    ("ai", "Sebastian Raschka", "https://magazine.sebastianraschka.com/feed", "en"),
    ("ai", "Lil'Log (Lilian Weng)", "https://lilianweng.github.io/index.xml", "en"),
    ("ai", "Chip Huyen Blog", "https://huyenchip.com/feed.xml", "en"),
    ("ai", "Import AI Newsletter", "https://importai.substack.com/feed", "en"),
    ("ai", "Simon Willison's Weblog", "https://simonwillison.net/atom/everything", "en"),
    ("ai", "Latent Space", "https://www.latent.space/feed", "en"),
    ("ai", "Mistral AI Blog", "https://mistral.ai/feed/", "en"),
    ("ai", "AI Snake Oil", "https://www.aisnakeoil.com/feed", "en"),
    ("ai", "Ahead of AI (Seb Raschka)", "https://magazine.sebastianraschka.com/feed", "en"),
    ("ai", "The Decoder", "https://the-decoder.com/feed/", "en"),
    ("ai", "Interconnects (Nathan Lambert)", "https://www.interconnects.ai/feed", "en"),
    ("ai", "Jack Clark (Import AI)", "https://jack-clark.net/feed/", "en"),
    // === Tech (16 feeds) ===
    ("tech", "TechCrunch", "https://techcrunch.com/feed/", "en"),
    ("tech", "The Verge", "https://www.theverge.com/rss/index.xml", "en"),
    ("tech", "Ars Technica", "https://feeds.arstechnica.com/arstechnica/index", "en"),
    ("tech", "Wired", "https://www.wired.com/feed/rss", "en"),
    ("tech", "Engadget", "https://www.engadget.com/rss.xml", "en"),
    ("tech", "Tom's Hardware", "https://www.tomshardware.com/feeds/all", "en"),
    ("tech", "Hacker News", "https://hnrss.org/frontpage", "en"),
    ("tech", "AnandTech", "https://www.anandtech.com/rss/", "en"),
    ("tech", "The Register", "https://www.theregister.com/headlines.atom", "en"),
    ("tech", "ZDNet", "https://www.zdnet.com/news/rss.xml", "en"),
    ("tech", "TechRadar", "https://www.techradar.com/rss", "en"),
    ("tech", "IEEE Spectrum", "https://spectrum.ieee.org/feeds/feed.rss", "en"),
    ("tech", "MIT Tech Review", "https://www.technologyreview.com/feed/", "en"),
    ("tech", "Ars Technica Gadgets", "https://feeds.arstechnica.com/arstechnica/gadgets", "en"),
    ("tech", "CNET", "https://www.cnet.com/rss/news/", "en"),
    ("tech", "9to5Mac", "https://9to5mac.com/feed/", "en"),
    ("tech", "9to5Google", "https://9to5google.com/feed/", "en"),
    ("tech", "Liliputing", "https://liliputing.com/feed/", "en"),
    ("tech", "Android Authority", "https://www.androidauthority.com/feed/", "en"),
    ("tech", "MacRumors", "https://feeds.macrumors.com/MacRumors-All", "en"),
    ("tech", "Protocol", "https://www.protocol.com/feeds/feed.rss", "en"),
    ("tech", "Slashdot", "https://rss.slashdot.org/Slashdot/slashdotMain", "en"),
    ("tech", "XDA Developers", "https://www.xda-developers.com/feed/", "en"),
    ("tech", "How-To Geek", "https://www.howtogeek.com/feed/", "en"),
    ("tech", "Ars Technica Science", "https://feeds.arstechnica.com/arstechnica/science", "en"),
    ("tech", "Daring Fireball", "https://daringfireball.net/feeds/main", "en"),
    ("tech", "404 Media", "https://www.404media.co/rss/", "en"),
    ("tech", "Platformer", "https://www.platformer.news/rss/", "en"),
    ("tech", "Rest of World", "https://restofworld.org/feed/latest/", "en"),
    ("tech", "Krebs on Security", "https://krebsonsecurity.com/feed/", "en"),
    ("tech", "GitHub Blog", "https://github.blog/feed/", "en"),
    ("tech", "Schneier on Security", "https://www.schneier.com/feed/", "en"),
    ("tech", "Product Hunt", "https://www.producthunt.com/feed", "en"),
    ("tech", "Y Combinator Blog", "https://www.ycombinator.com/blog/rss/", "en"),
    ("tech", "a]16z Blog", "https://a16z.com/feed/", "en"),
    ("tech", "Benedict Evans", "https://www.ben-evans.com/benedictevans?format=rss", "en"),
    ("tech", "Stratechery", "https://stratechery.com/feed/", "en"),
    ("tech", "The Information", "https://www.theinformation.com/feed", "en"),
    // === Italy (22 feeds) ===
    ("italy", "ANSA Politica", "https://www.ansa.it/sito/notizie/politica/politica_rss.xml", "it"),
    ("italy", "ANSA Cronaca", "https://www.ansa.it/sito/notizie/cronaca/cronaca_rss.xml", "it"),
    ("italy", "ANSA Economia", "https://www.ansa.it/sito/notizie/economia/economia_rss.xml", "it"),
    ("italy", "ANSA Sport", "https://www.ansa.it/sito/notizie/sport/sport_rss.xml", "it"),
    ("italy", "ANSA Cultura", "https://www.ansa.it/sito/notizie/cultura/cultura_rss.xml", "it"),
    ("italy", "ANSA Scienza", "https://www.ansa.it/canale_scienza_tecnica/scienza_rss.xml", "it"),
    ("italy", "La Repubblica", "https://www.repubblica.it/rss/homepage/rss2.0.xml", "it"),
    ("italy", "Corriere della Sera", "http://xml2.corriereobjects.it/rss/homepage.xml", "it"),
    ("italy", "Il Post", "https://www.ilpost.it/feed/", "it"),
    ("italy", "Il Sole 24 Ore", "https://www.ilsole24ore.com/rss/italia.xml", "it"),
    ("italy", "Il Fatto Quotidiano", "https://www.ilfattoquotidiano.it/feed/", "it"),
    ("italy", "Sky TG24", "https://tg24.sky.it/rss/tg24.xml", "it"),
    ("italy", "La Stampa", "https://www.lastampa.it/rss.xml", "it"),
    ("italy", "Il Messaggero", "https://www.ilmessaggero.it/rss/homepage.xml", "it"),
    ("italy", "Internazionale", "https://www.internazionale.it/feed", "it"),
    ("italy", "Fanpage", "https://www.fanpage.it/feed/", "it"),
    ("italy", "Wired Italia", "https://www.wired.it/feed/rss", "it"),
    ("italy", "AGI", "https://www.agi.it/rss/", "it"),
    ("italy", "RaiNews", "https://www.rainews.it/rss/tutti", "it"),
    ("italy", "Gazzetta dello Sport", "https://www.gazzetta.it/rss/home.xml", "it"),
    ("italy", "Domani", "https://www.editorialedomani.it/rss", "it"),
    ("italy", "Open Online", "https://www.open.online/feed/", "it"),
    ("italy", "ANSA Tecnologia", "https://www.ansa.it/sito/notizie/tecnologia/tecnologia_rss.xml", "it"),
    ("italy", "ANSA Mondo", "https://www.ansa.it/sito/notizie/mondo/mondo_rss.xml", "it"),
    ("italy", "HDblog", "https://www.hdblog.it/feed/", "it"),
    ("italy", "Startupitalia", "https://startupitalia.eu/feed", "it"),
    ("italy", "Linkiesta", "https://www.linkiesta.it/feed/", "it"),
    ("italy", "Il Foglio", "https://www.ilfoglio.it/rss.jsp", "it"),
    ("italy", "Quotidiano Nazionale", "https://www.quotidiano.net/rss/", "it"),
    ("italy", "TPI News", "https://www.tpi.it/feed/", "it"),
    ("italy", "Adnkronos", "https://www.adnkronos.com/rss", "it"),
    ("italy", "Libero Quotidiano", "https://www.liberoquotidiano.it/rss.xml", "it"),
    ("italy", "Valigia Blu", "https://www.valigiablu.it/feed/", "it"),
    // === Miami (13 feeds) ===
    ("miami", "WSVN Miami", "https://wsvn.com/feed", "en"),
    ("miami", "NBC 6 South Florida", "https://www.nbcmiami.com/news/local/feed/", "en"),
    ("miami", "Local10 WPLG", "https://www.local10.com/arc/outboundfeeds/rss/?outputType=xml", "en"),
    ("miami", "Miami Herald", "https://www.miamiherald.com/latest-news/index.rss", "en"),
    ("miami", "South Florida Sun Sentinel", "https://www.sun-sentinel.com/latest-news/index.rss", "en"),
    ("miami", "Miami New Times", "https://www.miaminewtimes.com/miami/Rss.xml", "en"),
    ("miami", "The Next Miami", "https://www.thenextmiami.com/feed/", "en"),
    ("miami", "WLRN Public Media", "https://www.wlrn.org/rss.xml", "en"),
    ("miami", "CBS Miami", "https://www.cbsnews.com/miami/latest/rss/main", "en"),
    ("miami", "Refresh Miami", "https://refreshmiami.com/feed/", "en"),
    ("miami", "Biscayne Times", "https://www.biscaynetimes.com/feed/", "en"),
    ("miami", "South Florida Business Journal", "https://www.bizjournals.com/southflorida/news/feed", "en"),
    ("miami", "Florida Politics", "https://floridapolitics.com/feed/", "en"),
    ("miami", "WTVJ NBC 6", "https://www.nbcmiami.com/news/feed/", "en"),
    ("miami", "Miami Today News", "https://www.miamitodaynews.com/feed/", "en"),
    ("miami", "Florida Insider", "https://floridainsider.com/feed/", "en"),
    ("miami", "South Florida Reporter", "https://southfloridareporter.com/feed/", "en"),
    ("miami", "Miami Agent Magazine", "https://miamiagentmagazine.com/feed/", "en"),
    ("miami", "Ocean Drive Magazine", "https://oceandrive.com/feed/", "en"),
    ("miami", "South Florida Tech Hub", "https://www.southfloridatechhub.com/feed/", "en"),
    ("miami", "The Real Deal Miami", "https://therealdeal.com/miami/feed/", "en"),
    ("miami", "WFOR CBS4 Miami", "https://www.cbsnews.com/miami/feed/", "en"),
    ("miami", "Florida Trend", "https://www.floridatrend.com/rss/", "en"),
    ("miami", "Patch Miami", "https://patch.com/florida/miami/rss", "en"),
    ("miami", "Miami Eater", "https://miami.eater.com/rss/index.xml", "en"),
    ("miami", "Curbed Miami", "https://miami.curbed.com/rss/index.xml", "en"),
    ("miami", "Miami Beach Chamber", "https://www.miamibeachchamber.com/feed/", "en"),
    ("miami", "South Beach Magazine", "https://southbeachmagazine.com/feed/", "en"),
    ("miami", "Miami Tech Works", "https://www.miamitechworks.com/feed/", "en"),
    // === Freedom: Time (10 feeds) ===
    ("freedom_time", "Cal Newport Blog", "https://calnewport.com/feed/", "en"),
    ("freedom_time", "Zen Habits", "https://zenhabits.net/feed/", "en"),
    ("freedom_time", "Asian Efficiency", "https://www.asianefficiency.com/feed/", "en"),
    ("freedom_time", "Doist Blog", "https://blog.doist.com/feed/", "en"),
    ("freedom_time", "Zapier Blog", "https://zapier.com/blog/feed/", "en"),
    ("freedom_time", "Lifehacker", "https://lifehacker.com/feed/rss", "en"),
    ("freedom_time", "James Clear", "https://jamesclear.com/feed", "en"),
    ("freedom_time", "Farnam Street", "https://fs.blog/feed/", "en"),
    ("freedom_time", "Ness Labs", "https://nesslabs.com/feed", "en"),
    ("freedom_time", "Indie Hackers", "https://www.indiehackers.com/feed.xml", "en"),
    ("freedom_time", "Todoist Blog", "https://blog.todoist.com/feed/", "en"),
    ("freedom_time", "Toggl Blog", "https://toggl.com/blog/feed", "en"),
    ("freedom_time", "Notion Blog", "https://www.notion.so/blog/rss.xml", "en"),
    ("freedom_time", "Basecamp Signal", "https://m.signalvnoise.com/feed/", "en"),
    ("freedom_time", "Tim Ferriss Blog", "https://tim.blog/feed/", "en"),
    ("freedom_time", "Effectiviology", "https://effectiviology.com/feed/", "en"),
    ("freedom_time", "Ali Abdaal Blog", "https://aliabdaal.com/feed/", "en"),
    ("freedom_time", "Cortex Newsletter", "https://newsletter.cortex.io/feed", "en"),
    ("freedom_time", "Reclaim AI Blog", "https://reclaim.ai/blog/rss.xml", "en"),
    ("freedom_time", "Focusmate Blog", "https://blog.focusmate.com/feed/", "en"),
    // === Freedom: Financial (10 feeds) ===
    ("freedom_wealth", "Mr Money Mustache", "https://www.mrmoneymustache.com/feed/", "en"),
    ("freedom_wealth", "Financial Samurai", "https://www.financialsamurai.com/feed/", "en"),
    ("freedom_wealth", "Of Dollars And Data", "https://ofdollarsanddata.com/feed/", "en"),
    ("freedom_wealth", "Motley Fool", "https://www.fool.com/feeds/index.aspx", "en"),
    ("freedom_wealth", "Seeking Alpha", "https://seekingalpha.com/feed.xml", "en"),
    ("freedom_wealth", "Investopedia", "https://www.investopedia.com/feedbuilder/feed/getfeed?feedName=rss_headline", "en"),
    ("freedom_wealth", "Bankrate", "https://www.bankrate.com/feed/", "en"),
    ("freedom_wealth", "Stratechery", "https://stratechery.com/feed/", "en"),
    ("freedom_wealth", "SaaStr Blog", "https://www.saastr.com/feed/", "en"),
    ("freedom_wealth", "Bootstrapped Founder", "https://thebootstrappedfounder.com/feed/", "en"),
    ("freedom_wealth", "Kiplinger", "https://www.kiplinger.com/feed/all", "en"),
    ("freedom_wealth", "NerdWallet Blog", "https://www.nerdwallet.com/blog/feed/", "en"),
    ("freedom_wealth", "The Hustle", "https://thehustle.co/feed/", "en"),
    ("freedom_wealth", "Morning Brew", "https://www.morningbrew.com/daily/rss", "en"),
    ("freedom_wealth", "Abnormal Returns", "https://abnormalreturns.com/feed/", "en"),
    ("freedom_wealth", "A Wealth of Common Sense", "https://awealthofcommonsense.com/feed/", "en"),
    ("freedom_wealth", "The Irrelevant Investor", "https://theirrelevantinvestor.com/feed/", "en"),
    ("freedom_wealth", "Marker (Medium)", "https://marker.medium.com/feed", "en"),
    ("freedom_wealth", "Collaborative Fund Blog", "https://www.collaborativefund.com/feed.xml", "en"),
    ("freedom_wealth", "Finimize", "https://www.finimize.com/wp/feed/", "en"),
    // === Freedom: Location (10 feeds) ===
    ("freedom_location", "Nomad List Blog", "https://nomadlist.com/blog.rss", "en"),
    ("freedom_location", "Remote OK Blog", "https://remoteok.com/blog.rss", "en"),
    ("freedom_location", "Goats on the Road", "https://www.goatsontheroad.com/feed/", "en"),
    ("freedom_location", "Nomadic Matt", "https://www.nomadicmatt.com/travel-blog/feed/", "en"),
    ("freedom_location", "The Points Guy", "https://thepointsguy.com/feed/", "en"),
    ("freedom_location", "Remote.co Blog", "https://remote.co/feed/", "en"),
    ("freedom_location", "We Work Remotely Blog", "https://weworkremotely.com/blog.rss", "en"),
    ("freedom_location", "Running Remote", "https://runningremote.com/feed/", "en"),
    ("freedom_location", "Expatica", "https://www.expatica.com/feed/", "en"),
    ("freedom_location", "International Living", "https://internationalliving.com/feed/", "en"),
    ("freedom_location", "Lonely Planet", "https://www.lonelyplanet.com/feed.xml", "en"),
    ("freedom_location", "FlexJobs Blog", "https://www.flexjobs.com/blog/feed/", "en"),
    ("freedom_location", "Pieter Levels Blog", "https://levels.io/rss", "en"),
    ("freedom_location", "Travel Lemming", "https://travellemming.com/feed/", "en"),
    ("freedom_location", "Location Rebel", "https://www.locationrebel.com/feed/", "en"),
    ("freedom_location", "The Remote Company", "https://theremotecompany.com/blog/feed/", "en"),
    ("freedom_location", "Too Many Adapters", "https://toomanyadapters.com/feed/", "en"),
    ("freedom_location", "Boundless Life Blog", "https://www.boundless.life/blog/feed/", "en"),
    ("freedom_location", "SafetyWing Blog", "https://safetywing.com/blog/feed/", "en"),
    ("freedom_location", "Anyplace Blog", "https://www.anyplace.com/blog/feed/", "en"),
    // === Freedom: Health (10 feeds) ===
    ("freedom_health", "Peter Attia", "https://peterattiamd.com/feed/", "en"),
    ("freedom_health", "Huberman Lab", "https://www.hubermanlab.com/rss", "en"),
    ("freedom_health", "Examine.com Blog", "https://examine.com/blog/feed/", "en"),
    ("freedom_health", "Mark's Daily Apple", "https://www.marksdailyapple.com/feed/", "en"),
    ("freedom_health", "Precision Nutrition", "https://www.precisionnutrition.com/feed", "en"),
    ("freedom_health", "Ben Greenfield Life", "https://bengreenfieldlife.com/feed/", "en"),
    ("freedom_health", "Stronger by Science", "https://www.strongerbyscience.com/feed/", "en"),
    ("freedom_health", "Nerd Fitness", "https://www.nerdfitness.com/blog/feed/", "en"),
    ("freedom_health", "Longevity.Technology", "https://longevity.technology/feed/", "en"),
    ("freedom_health", "Life Extension Blog", "https://www.lifeextension.com/blog/feed", "en"),
    ("freedom_health", "Rhonda Patrick (FoundMyFitness)", "https://www.foundmyfitness.com/feed.xml", "en"),
    ("freedom_health", "Examine.com Digest", "https://examine.com/digest/feed/", "en"),
    ("freedom_health", "Barbell Medicine", "https://www.barbellmedicine.com/feed/", "en"),
    ("freedom_health", "Levels Health Blog", "https://www.levels.com/blog/feed", "en"),
    ("freedom_health", "InsideTracker Blog", "https://blog.insidetracker.com/rss.xml", "en"),
    ("freedom_health", "Healthline Nutrition", "https://www.healthline.com/nutrition/feed", "en"),
    ("freedom_health", "MindBodyGreen", "https://www.mindbodygreen.com/rss", "en"),
    ("freedom_health", "Well+Good", "https://www.wellandgood.com/feed/", "en"),
    ("freedom_health", "Bulletproof Blog", "https://www.bulletproof.com/blog/feed/", "en"),
    ("freedom_health", "Lifespan.io", "https://www.lifespan.io/feed/", "en"),
];

pub async fn fetch_all() -> anyhow::Result<Vec<RawArticle>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let futures: Vec<_> = FEEDS
        .iter()
        .map(|(sector, name, url, lang)| {
            let client = client.clone();
            let sector = sector.to_string();
            let name = name.to_string();
            let url = url.to_string();
            let lang = lang.to_string();
            async move { fetch_single(&client, &sector, &name, &url, &lang).await }
        })
        .collect();

    let results = futures::future::join_all(futures).await;
    let mut all_articles = Vec::new();

    for result in results {
        match result {
            Ok(articles) => all_articles.extend(articles),
            Err(e) => tracing::warn!("RSS fetch error: {}", e),
        }
    }

    Ok(all_articles)
}

async fn fetch_single(
    client: &reqwest::Client,
    sector: &str,
    name: &str,
    url: &str,
    lang: &str,
) -> anyhow::Result<Vec<RawArticle>> {
    let response = client
        .get(url)
        .header("User-Agent", "Pulse/0.1")
        .send()
        .await?
        .bytes()
        .await?;

    let feed = feed_rs::parser::parse(&response[..])?;
    let mut articles = Vec::new();

    // Take articles from the last 48 hours (pipeline runs at 8 AM + 9 PM,
    // 48h covers timezone gaps and feeds with stale pub dates)
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(48);

    for entry in feed.entries.iter().take(20) {
        let pub_date = entry.published.or(entry.updated);

        // Skip old articles if we have a date
        if let Some(dt) = pub_date {
            if dt < cutoff {
                continue;
            }
        }

        let title = entry.title.as_ref().map(|t| t.content.clone()).unwrap_or_default();
        let link = entry
            .links
            .first()
            .map(|l| l.href.clone())
            .unwrap_or_default();
        let snippet = entry
            .summary
            .as_ref()
            .map(|s| s.content.clone())
            .or_else(|| {
                entry.content.as_ref().and_then(|c| c.body.clone())
            })
            .unwrap_or_default();

        if !title.is_empty() && !link.is_empty() {
            articles.push(RawArticle {
                title,
                url: link,
                source_name: name.to_string(),
                source_url: url.to_string(),
                published_at: pub_date.map(|dt| dt.to_rfc3339()),
                content_snippet: snippet.chars().take(500).collect(),
                sector: sector.to_string(),
                feed_id: format!("rss_{}", name.to_lowercase().replace(' ', "_")),
                language: lang.to_string(),
            });
        }
    }

    tracing::info!("Fetched {} articles from {}", articles.len(), name);
    Ok(articles)
}
