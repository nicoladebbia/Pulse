use super::RawArticle;

const FEEDS: &[(&str, &str, &str)] = &[
    // === AI (12 queries) ===
    ("ai", "Google News AI Companies", "https://news.google.com/rss/search?q=OpenAI+OR+Anthropic+OR+ChatGPT+OR+Claude+OR+%22Google+Gemini%22+OR+%22Meta+AI%22+OR+Mistral+OR+xAI+OR+Grok+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("ai", "Google News AI Products", "https://news.google.com/rss/search?q=%22AI+model%22+OR+%22GPT-5%22+OR+%22new+AI%22+OR+%22AI+agent%22+OR+%22AI+tool%22+OR+%22AI+launch%22+OR+%22AI+release%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("ai", "Google News AI Regulation", "https://news.google.com/rss/search?q=%22AI+regulation%22+OR+%22AI+safety%22+OR+%22AI+policy%22+OR+%22AI+ethics%22+OR+%22AI+governance%22+OR+%22EU+AI+Act%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("ai", "Google News AI Coding", "https://news.google.com/rss/search?q=%22AI+coding%22+OR+%22GitHub+Copilot%22+OR+%22Cursor+IDE%22+OR+%22Claude+Code%22+OR+%22Devin+AI%22+OR+%22vibe+coding%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("ai", "Google News AI Research", "https://news.google.com/rss/search?q=%22AI+breakthrough%22+OR+%22AI+research%22+OR+%22large+language+model%22+OR+%22foundation+model%22+OR+%22reasoning+model%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("ai", "Google News AI Startups", "https://news.google.com/rss/search?q=%22AI+startup%22+OR+%22AI+funding%22+OR+%22AI+unicorn%22+OR+%22AI+acquisition%22+OR+%22AI+valuation%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("ai", "Google News AI Enterprise", "https://news.google.com/rss/search?q=%22enterprise+AI%22+OR+%22AI+deployment%22+OR+%22AI+infrastructure%22+OR+%22AI+chips%22+OR+%22AI+data+center%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("ai", "Google News AI Open Source", "https://news.google.com/rss/search?q=%22open+source+AI%22+OR+%22open+weights%22+OR+Llama+OR+%22Stable+Diffusion%22+OR+%22Hugging+Face%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("ai", "Google News AI Robotics", "https://news.google.com/rss/search?q=%22AI+robotics%22+OR+%22embodied+AI%22+OR+%22Figure+robot%22+OR+%22Tesla+Optimus%22+OR+%22humanoid+robot%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("ai", "Google News AI Image Video", "https://news.google.com/rss/search?q=%22AI+image%22+OR+%22AI+video%22+OR+Midjourney+OR+Sora+OR+%22generative+AI%22+OR+DALL-E+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("ai", "Google News AI Healthcare", "https://news.google.com/rss/search?q=%22AI+healthcare%22+OR+%22AI+drug+discovery%22+OR+%22AI+diagnosis%22+OR+%22medical+AI%22+OR+AlphaFold+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("ai", "Google News AI Jobs", "https://news.google.com/rss/search?q=%22AI+jobs%22+OR+%22AI+replacing%22+OR+%22AI+workforce%22+OR+%22AI+hiring%22+OR+%22AI+layoffs%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    // === Miami (14 queries) ===
    ("miami", "Google News Miami", "https://news.google.com/rss/search?q=%22Miami+Beach%22+OR+%22South+Beach%22+OR+%22Miami-Dade%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("miami", "Google News Miami Local", "https://news.google.com/rss/search?q=%22Miami+Beach%22+development+OR+construction+OR+events+OR+restaurant+OR+nightlife+OR+tourism+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("miami", "Google News South Florida", "https://news.google.com/rss/search?q=%22South+Florida%22+OR+%22Brickell%22+OR+%22Wynwood%22+OR+%22Coconut+Grove%22+OR+%22Fort+Lauderdale%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("miami", "Google News Miami Real Estate", "https://news.google.com/rss/search?q=Miami+real+estate+OR+%22Miami+condo%22+OR+%22Miami+housing%22+OR+%22Miami+rent%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("miami", "Google News Miami Tech", "https://news.google.com/rss/search?q=%22Miami+tech%22+OR+%22Miami+startup%22+OR+%22Magic+City+Innovation%22+OR+%22Miami+VC%22+OR+%22South+Florida+tech%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("miami", "Google News Miami Culture", "https://news.google.com/rss/search?q=%22Art+Basel+Miami%22+OR+%22Miami+music%22+OR+%22Miami+art%22+OR+%22Perez+Art+Museum%22+OR+%22Design+District%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("miami", "Google News Miami Food", "https://news.google.com/rss/search?q=%22Miami+restaurant%22+OR+%22Miami+food%22+OR+%22Miami+chef%22+OR+%22Little+Havana%22+food+OR+%22Miami+dining%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("miami", "Google News Miami Sports", "https://news.google.com/rss/search?q=%22Miami+Heat%22+OR+%22Miami+Dolphins%22+OR+%22Inter+Miami%22+OR+%22Florida+Panthers%22+OR+%22Miami+Marlins%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("miami", "Google News Miami Weather", "https://news.google.com/rss/search?q=Miami+hurricane+OR+%22Miami+flooding%22+OR+%22South+Florida+weather%22+OR+%22Miami+climate%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("miami", "Google News Miami Business", "https://news.google.com/rss/search?q=%22Miami+business%22+OR+%22Miami+economy%22+OR+%22Miami+jobs%22+OR+%22Port+of+Miami%22+OR+%22Miami+crypto%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("miami", "Google News Miami Transport", "https://news.google.com/rss/search?q=%22Miami+traffic%22+OR+%22Brightline%22+Miami+OR+%22Miami+airport%22+OR+%22Miami+transit%22+OR+%22I-95+Miami%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("miami", "Google News Miami Immigration", "https://news.google.com/rss/search?q=Miami+immigration+OR+%22Miami+Cuban%22+OR+%22Miami+Latin+America%22+OR+%22Miami+expat%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("miami", "Google News Florida Politics", "https://news.google.com/rss/search?q=%22Florida+governor%22+OR+%22DeSantis%22+OR+%22Florida+law%22+OR+%22Florida+politics%22+Miami+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("miami", "Google News Miami Luxury", "https://news.google.com/rss/search?q=%22Miami+luxury%22+OR+%22Fisher+Island%22+OR+%22Star+Island%22+OR+%22Miami+yacht%22+OR+%22Miami+penthouse%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    // === Italy (16 queries) ===
    ("italy", "Google News Italy Today", "https://news.google.com/rss/search?q=Italy+news+today+when:1d&hl=en&gl=US&ceid=US:en"),
    ("italy", "Google News Italy Politics", "https://news.google.com/rss/search?q=Italy+politics+OR+Italian+government+OR+Meloni+OR+Italian+parliament+when:1d&hl=en&gl=US&ceid=US:en"),
    ("italy", "Google News Italy Economy", "https://news.google.com/rss/search?q=Italian+economy+OR+%22Bank+of+Italy%22+OR+%22Made+in+Italy%22+OR+Italian+business+OR+Italian+startup+when:1d&hl=en&gl=US&ceid=US:en"),
    ("italy", "Google News Italy Culture", "https://news.google.com/rss/search?q=Italy+culture+OR+Italian+food+OR+Italian+fashion+OR+Italian+travel+OR+Italian+art+OR+Vatican+OR+Pope+when:1d&hl=en&gl=US&ceid=US:en"),
    ("italy", "Google News Serie A", "https://news.google.com/rss/search?q=%22Serie+A%22+OR+%22Italian+football%22+OR+%22Juventus%22+OR+%22Inter+Milan%22+OR+%22AC+Milan%22+OR+%22Napoli%22+OR+%22Roma%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("italy", "Google News Italy Science", "https://news.google.com/rss/search?q=Italy+science+OR+Italian+research+OR+Italian+space+OR+Italian+technology+OR+Italian+innovation+when:1d&hl=en&gl=US&ceid=US:en"),
    ("italy", "Google News Italy Tourism", "https://news.google.com/rss/search?q=%22Italy+tourism%22+OR+%22Italian+travel%22+OR+%22visit+Italy%22+OR+%22Italian+hotels%22+OR+%22Amalfi%22+OR+%22Tuscany%22+when:1d&hl=en&gl=US&ceid=US:en"),
    ("italy", "Google News Italy Luxury", "https://news.google.com/rss/search?q=%22Italian+luxury%22+OR+Ferrari+OR+Lamborghini+OR+Gucci+OR+Prada+OR+%22Italian+design%22+when:1d&hl=en&gl=US&ceid=US:en"),
    ("italy", "Google News Italy Energy", "https://news.google.com/rss/search?q=%22Italy+energy%22+OR+%22Italian+renewable%22+OR+Enel+OR+Eni+OR+%22Italian+gas%22+OR+%22Italy+climate%22+when:1d&hl=en&gl=US&ceid=US:en"),
    ("italy", "Google News Italy Immigration", "https://news.google.com/rss/search?q=%22Italy+migration%22+OR+%22Italian+immigration%22+OR+%22Lampedusa%22+OR+%22Mediterranean+migrants%22+Italy+when:1d&hl=en&gl=US&ceid=US:en"),
    ("italy", "Google News Italy EU", "https://news.google.com/rss/search?q=Italy+EU+OR+%22Italy+European+Union%22+OR+%22Italian+EU%22+OR+Italy+NATO+OR+%22Italian+foreign+policy%22+when:1d&hl=en&gl=US&ceid=US:en"),
    ("italy", "Google News Italy Crime", "https://news.google.com/rss/search?q=%22Italian+mafia%22+OR+%22Italy+crime%22+OR+%22Ndrangheta%22+OR+%22Camorra%22+OR+%22Italian+court%22+when:1d&hl=en&gl=US&ceid=US:en"),
    ("italy", "Google News Italy Health", "https://news.google.com/rss/search?q=%22Italy+health%22+OR+%22Italian+healthcare%22+OR+%22Italian+hospital%22+OR+%22Italian+medicine%22+when:1d&hl=en&gl=US&ceid=US:en"),
    ("italy", "Google News Italy Startups", "https://news.google.com/rss/search?q=%22Italian+startup%22+OR+%22Italy+tech%22+OR+%22Italian+fintech%22+OR+%22Italian+innovation%22+OR+%22Milan+startup%22+when:1d&hl=en&gl=US&ceid=US:en"),
    ("italy", "Google News Italy Real Estate", "https://news.google.com/rss/search?q=%22Italy+real+estate%22+OR+%22Italian+property%22+OR+%22buy+house+Italy%22+OR+%22Italian+housing%22+OR+%221+euro+house%22+when:1d&hl=en&gl=US&ceid=US:en"),
    ("italy", "Google News Italy Education", "https://news.google.com/rss/search?q=%22Italy+university%22+OR+%22Italian+education%22+OR+%22Bocconi%22+OR+%22Politecnico+Milano%22+OR+%22Italian+students%22+when:1d&hl=en&gl=US&ceid=US:en"),
    // === Tech (16 queries) ===
    ("tech", "Google News Tech", "https://news.google.com/rss/search?q=technology+innovation+startup+electronics+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("tech", "Google News Tech Products", "https://news.google.com/rss/search?q=%22price+drop%22+OR+%22price+cut%22+OR+%22new+release%22+OR+%22just+launched%22+hardware+OR+gadget+OR+GPU+OR+laptop+OR+phone+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("tech", "Google News Hardware", "https://news.google.com/rss/search?q=chip+OR+processor+OR+GPU+OR+robotics+OR+EV+OR+wearable+OR+display+new+OR+launch+OR+review+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("tech", "Google News Smart Home", "https://news.google.com/rss/search?q=%22smart+home%22+OR+%22smart+fridge%22+OR+%22smart+appliance%22+OR+%22home+automation%22+OR+Ring+OR+Nest+OR+%22Matter+protocol%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("tech", "Google News Gadgets", "https://news.google.com/rss/search?q=gadget+review+OR+%22hands+on%22+OR+%22first+look%22+OR+%22product+launch%22+OR+drone+OR+headphones+OR+smartwatch+OR+fitness+tracker+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("tech", "Google News Robotics EVs", "https://news.google.com/rss/search?q=robot+OR+humanoid+OR+%22electric+vehicle%22+OR+Tesla+OR+Rivian+OR+%223D+printing%22+OR+%22quantum+computer%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("tech", "Google News Cybersecurity", "https://news.google.com/rss/search?q=cybersecurity+OR+%22data+breach%22+OR+%22zero+day%22+OR+ransomware+OR+%22cyber+attack%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("tech", "Google News Space Tech", "https://news.google.com/rss/search?q=SpaceX+OR+%22Blue+Origin%22+OR+NASA+OR+%22space+launch%22+OR+Starship+OR+%22satellite+internet%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("tech", "Google News Apple", "https://news.google.com/rss/search?q=Apple+iPhone+OR+MacBook+OR+%22Apple+Vision%22+OR+%22Apple+AI%22+OR+iOS+OR+WWDC+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("tech", "Google News Semiconductors", "https://news.google.com/rss/search?q=TSMC+OR+NVIDIA+OR+AMD+OR+Intel+OR+%22chip+shortage%22+OR+%22semiconductor%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("tech", "Google News Crypto Web3", "https://news.google.com/rss/search?q=Bitcoin+OR+Ethereum+OR+%22crypto+regulation%22+OR+blockchain+OR+%22Web3%22+OR+DeFi+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("tech", "Google News Cloud Computing", "https://news.google.com/rss/search?q=%22cloud+computing%22+OR+AWS+OR+%22Google+Cloud%22+OR+Azure+OR+%22serverless%22+OR+Kubernetes+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("tech", "Google News Gaming", "https://news.google.com/rss/search?q=%22gaming+news%22+OR+%22PS5%22+OR+%22Xbox%22+OR+%22Nintendo+Switch%22+OR+%22Steam+Deck%22+OR+%22game+release%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("tech", "Google News Biotech", "https://news.google.com/rss/search?q=biotech+OR+CRISPR+OR+%22gene+therapy%22+OR+%22synthetic+biology%22+OR+%22lab+grown%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("tech", "Google News Open Source", "https://news.google.com/rss/search?q=%22open+source%22+software+OR+Linux+OR+%22developer+tools%22+OR+Rust+OR+%22programming+language%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("tech", "Google News VR AR", "https://news.google.com/rss/search?q=%22virtual+reality%22+OR+%22augmented+reality%22+OR+%22Meta+Quest%22+OR+%22Apple+Vision+Pro%22+OR+XR+when:1d&hl=en-US&gl=US&ceid=US:en"),
    // === FOUR FREEDOMS — Time (12 queries) ===
    ("freedom_time", "Freedom: Productivity", "https://news.google.com/rss/search?q=productivity+tools+OR+%22time+management%22+OR+%22work+automation%22+OR+%22passive+income%22+OR+%224-day+work+week%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_time", "Freedom: AI Delegation", "https://news.google.com/rss/search?q=%22AI+assistant%22+OR+%22AI+automation%22+OR+solopreneur+OR+%22async+work%22+OR+%22calendar+optimization%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_time", "Freedom: Work Life Balance", "https://news.google.com/rss/search?q=%22work+life+balance%22+OR+%22burnout+prevention%22+OR+%22flexible+schedule%22+OR+%22sabbatical%22+OR+%22time+off%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_time", "Freedom: No-Code Tools", "https://news.google.com/rss/search?q=%22no+code%22+OR+%22low+code%22+OR+%22workflow+automation%22+OR+Notion+OR+Airtable+OR+Make.com+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_time", "Freedom: Creator Economy", "https://news.google.com/rss/search?q=%22creator+economy%22+OR+%22one+person+business%22+OR+%22solo+founder%22+OR+%22micro+SaaS%22+OR+%22build+in+public%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_time", "Freedom: Deep Work", "https://news.google.com/rss/search?q=%22deep+work%22+OR+%22attention+management%22+OR+%22digital+minimalism%22+OR+%22focus+techniques%22+OR+%22flow+state%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_time", "Freedom: Outsourcing", "https://news.google.com/rss/search?q=%22virtual+assistant%22+OR+%22outsource+tasks%22+OR+%22delegate+work%22+OR+%22hire+freelancer%22+OR+Upwork+Fiverr+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_time", "Freedom: Systems Thinking", "https://news.google.com/rss/search?q=%22systems+thinking%22+OR+%22second+brain%22+OR+%22personal+knowledge+management%22+OR+Obsidian+OR+Roam+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_time", "Freedom: Retirement Early", "https://news.google.com/rss/search?q=%22early+retirement%22+OR+%22retire+early%22+OR+%22FIRE+lifestyle%22+OR+%22financial+independence+retire%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_time", "Freedom: Automation News", "https://news.google.com/rss/search?q=%22business+automation%22+OR+%22automate+everything%22+OR+%22RPA%22+OR+%22process+automation%22+OR+%22AI+workflow%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_time", "Freedom: Minimalism", "https://news.google.com/rss/search?q=minimalism+lifestyle+OR+%22simple+living%22+OR+%22intentional+living%22+OR+declutter+OR+essentialism+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_time", "Freedom: Async Culture", "https://news.google.com/rss/search?q=%22async+communication%22+OR+%22meeting+free%22+OR+%22no+meetings%22+OR+%22asynchronous+work%22+OR+%22remote+first%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    // === FOUR FREEDOMS — Financial (12 queries) ===
    ("freedom_wealth", "Freedom: Investing", "https://news.google.com/rss/search?q=investing+strategy+OR+%22stock+market%22+today+OR+crypto+news+OR+%22real+estate+investing%22+OR+%22FIRE+movement%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_wealth", "Freedom: Business", "https://news.google.com/rss/search?q=bootstrapping+OR+%22SaaS+revenue%22+OR+%22startup+funding%22+OR+%22dividend+income%22+OR+%22personal+finance%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_wealth", "Freedom: Passive Income", "https://news.google.com/rss/search?q=%22passive+income%22+OR+%22income+streams%22+OR+%22rental+income%22+OR+%22royalty+income%22+OR+%22affiliate+marketing%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_wealth", "Freedom: Crypto DeFi", "https://news.google.com/rss/search?q=%22crypto+investing%22+OR+%22DeFi+yield%22+OR+%22Bitcoin+strategy%22+OR+%22Ethereum+staking%22+OR+%22crypto+portfolio%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_wealth", "Freedom: Real Estate", "https://news.google.com/rss/search?q=%22real+estate+investing%22+OR+%22rental+property%22+OR+%22Airbnb+host%22+OR+%22house+hacking%22+OR+REIT+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_wealth", "Freedom: Tax Strategy", "https://news.google.com/rss/search?q=%22tax+strategy%22+OR+%22tax+optimization%22+OR+%22tax+free%22+OR+%22LLC+taxes%22+OR+%22capital+gains+tax%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_wealth", "Freedom: Indie SaaS", "https://news.google.com/rss/search?q=%22indie+hacker%22+OR+%22micro+SaaS%22+OR+%22bootstrapped+startup%22+OR+%22MRR+milestone%22+OR+%22solo+SaaS%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_wealth", "Freedom: Wealth Building", "https://news.google.com/rss/search?q=%22wealth+building%22+OR+%22net+worth%22+OR+%22compound+interest%22+OR+%22index+fund%22+OR+%22financial+freedom%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_wealth", "Freedom: Side Hustles", "https://news.google.com/rss/search?q=%22side+hustle%22+OR+%22freelance+income%22+OR+%22gig+economy%22+OR+%22monetize+skills%22+OR+%22online+business%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_wealth", "Freedom: Market Analysis", "https://news.google.com/rss/search?q=%22market+analysis%22+OR+%22S%26P+500%22+OR+%22market+crash%22+OR+%22bull+market%22+OR+%22recession+risk%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_wealth", "Freedom: Fintech", "https://news.google.com/rss/search?q=fintech+OR+neobank+OR+%22digital+banking%22+OR+Robinhood+OR+%22payment+platform%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_wealth", "Freedom: Venture Capital", "https://news.google.com/rss/search?q=%22venture+capital%22+OR+%22angel+investing%22+OR+%22seed+round%22+OR+%22Series+A%22+OR+%22startup+valuation%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    // === FOUR FREEDOMS — Location (12 queries) ===
    ("freedom_location", "Freedom: Remote Work", "https://news.google.com/rss/search?q=%22remote+work%22+OR+%22digital+nomad%22+OR+%22work+from+anywhere%22+OR+%22visa+program%22+OR+coworking+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_location", "Freedom: Global Mobility", "https://news.google.com/rss/search?q=%22cost+of+living%22+comparison+OR+%22relocation+incentive%22+OR+geo-arbitrage+OR+Starlink+OR+%22citizenship+by+investment%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_location", "Freedom: Nomad Visas", "https://news.google.com/rss/search?q=%22digital+nomad+visa%22+OR+%22remote+work+visa%22+OR+%22freelancer+visa%22+OR+%22Portugal+visa%22+OR+%22Spain+visa%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_location", "Freedom: Expat Life", "https://news.google.com/rss/search?q=%22expat+life%22+OR+%22living+abroad%22+OR+%22move+abroad%22+OR+%22best+countries+to+live%22+OR+%22expat+community%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_location", "Freedom: Coworking Coliving", "https://news.google.com/rss/search?q=coworking+OR+coliving+OR+%22coworking+space%22+OR+%22digital+nomad+hub%22+OR+%22remote+work+retreat%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_location", "Freedom: Second Passport", "https://news.google.com/rss/search?q=%22second+passport%22+OR+%22dual+citizenship%22+OR+%22golden+visa%22+OR+%22residency+by+investment%22+OR+%22citizenship+program%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_location", "Freedom: Best Cities Remote", "https://news.google.com/rss/search?q=%22best+cities+remote+work%22+OR+%22cheapest+cities%22+OR+%22safest+countries%22+OR+%22quality+of+life%22+ranking+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_location", "Freedom: Travel Hacking", "https://news.google.com/rss/search?q=%22travel+hacking%22+OR+%22credit+card+points%22+OR+%22flight+deals%22+OR+%22airline+miles%22+OR+%22travel+rewards%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_location", "Freedom: Global Tax", "https://news.google.com/rss/search?q=%22tax+residency%22+OR+%22offshore+company%22+OR+%22territorial+tax%22+OR+%22tax+haven%22+OR+%22expat+tax%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_location", "Freedom: Slow Travel", "https://news.google.com/rss/search?q=%22slow+travel%22+OR+%22long+term+travel%22+OR+%22house+sitting%22+OR+%22workation%22+OR+%22travel+while+working%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_location", "Freedom: Infrastructure", "https://news.google.com/rss/search?q=Starlink+OR+%22global+internet%22+OR+%22eSIM+travel%22+OR+%22VPN+remote%22+OR+%22portable+office%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_location", "Freedom: Immigration Policy", "https://news.google.com/rss/search?q=%22immigration+policy%22+change+OR+%22visa+update%22+OR+%22border+policy%22+OR+%22work+permit%22+new+when:1d&hl=en-US&gl=US&ceid=US:en"),
    // === FOUR FREEDOMS — Health (12 queries) ===
    ("freedom_health", "Freedom: Longevity", "https://news.google.com/rss/search?q=longevity+OR+biohacking+OR+%22sleep+science%22+OR+peptides+OR+%22intermittent+fasting%22+OR+%22founder+burnout%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_health", "Freedom: Fitness Tech", "https://news.google.com/rss/search?q=%22fitness+tracker%22+OR+%22continuous+glucose+monitor%22+OR+Whoop+OR+Oura+OR+%22cold+plunge%22+OR+sauna+health+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_health", "Freedom: Sleep Optimization", "https://news.google.com/rss/search?q=%22sleep+optimization%22+OR+%22sleep+quality%22+OR+%22circadian+rhythm%22+OR+%22sleep+tracker%22+OR+melatonin+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_health", "Freedom: Nutrition Science", "https://news.google.com/rss/search?q=%22nutrition+science%22+OR+%22gut+health%22+OR+microbiome+OR+%22anti-inflammatory+diet%22+OR+%22protein+intake%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_health", "Freedom: Mental Health", "https://news.google.com/rss/search?q=%22mental+health%22+tech+OR+%22meditation+app%22+OR+%22therapy+AI%22+OR+%22stress+management%22+OR+psychedelics+therapy+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_health", "Freedom: Strength Training", "https://news.google.com/rss/search?q=%22strength+training%22+OR+%22resistance+training%22+OR+%22muscle+building%22+OR+%22home+gym%22+OR+CrossFit+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_health", "Freedom: Anti-Aging", "https://news.google.com/rss/search?q=%22anti+aging%22+OR+%22age+reversal%22+OR+%22Bryan+Johnson%22+OR+NMN+OR+NAD+OR+rapamycin+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_health", "Freedom: Wearable Health", "https://news.google.com/rss/search?q=%22wearable+health%22+OR+%22Apple+Watch+health%22+OR+%22blood+pressure+watch%22+OR+%22health+monitoring%22+OR+%22smart+ring%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_health", "Freedom: Preventive Medicine", "https://news.google.com/rss/search?q=%22preventive+medicine%22+OR+%22health+screening%22+OR+%22early+detection%22+OR+%22blood+test%22+biomarker+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_health", "Freedom: Nootropics", "https://news.google.com/rss/search?q=nootropics+OR+%22cognitive+enhancement%22+OR+%22brain+health%22+OR+%22focus+supplement%22+OR+%22smart+drugs%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_health", "Freedom: Running Endurance", "https://news.google.com/rss/search?q=marathon+OR+%22endurance+training%22+OR+%22zone+2+training%22+OR+%22VO2+max%22+OR+%22running+science%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
    ("freedom_health", "Freedom: Recovery", "https://news.google.com/rss/search?q=%22muscle+recovery%22+OR+%22ice+bath%22+OR+%22red+light+therapy%22+OR+%22compression+therapy%22+OR+%22sports+recovery%22+when:1d&hl=en-US&gl=US&ceid=US:en"),
];

pub async fn fetch_all() -> anyhow::Result<Vec<RawArticle>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;
    let mut all_articles = Vec::new();

    let futures: Vec<_> = FEEDS
        .iter()
        .map(|(sector, name, url)| {
            let client = client.clone();
            let sector = sector.to_string();
            let name = name.to_string();
            let url = url.to_string();
            async move { fetch_feed(&client, &sector, &name, &url).await }
        })
        .collect();

    let results = futures::future::join_all(futures).await;
    for result in results {
        match result {
            Ok(articles) => all_articles.extend(articles),
            Err(e) => tracing::warn!("Google News fetch error: {}", e),
        }
    }

    Ok(all_articles)
}

async fn fetch_feed(
    client: &reqwest::Client,
    sector: &str,
    name: &str,
    url: &str,
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

    for entry in feed.entries.iter().take(25) {
        let title = entry.title.as_ref().map(|t| t.content.clone()).unwrap_or_default();
        let link = entry
            .links
            .first()
            .map(|l| l.href.clone())
            .unwrap_or_default();
        let published = entry
            .published
            .or(entry.updated)
            .map(|dt| dt.to_rfc3339());
        let snippet = entry
            .summary
            .as_ref()
            .map(|s| s.content.clone())
            .unwrap_or_default();

        if !title.is_empty() && !link.is_empty() {
            articles.push(RawArticle {
                title,
                url: link,
                source_name: name.to_string(),
                source_url: url.to_string(),
                published_at: published,
                content_snippet: snippet,
                sector: sector.to_string(),
                feed_id: format!("google_news_{}", sector),
                language: "en".to_string(),
            });
        }
    }

    tracing::info!("Fetched {} articles from {}", articles.len(), name);
    Ok(articles)
}
