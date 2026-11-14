# Your first clean

## The short way: Health check

Zest opens on **Health check**. It looks at four things and changes nothing while it looks:

| | What it checks |
|---|---|
| **Privacy** | Trackers (third-party cookies) in your browsers. Sites on your keep list are not counted. |
| **Space** | Junk that can be removed: temporary files, caches, logs, the Recycle Bin. |
| **Speed** | Programs that start with Windows. |
| **Security** | Installed programs that have updates waiting. |

The bar at the top shows your system drive: the solid part is in use, the hatched part is what a
clean would give back.

Press **Make it better**. Zest tells you what it is about to remove and asks you to confirm. It
then cleans Privacy and Space. Speed and Security are never changed automatically: their buttons
take you to the Startup and Software updates pages so you decide.

## The thorough way: Custom clean

**Custom clean** is the same engine with every switch exposed.

1. On the left, rules are grouped: Windows, each browser you have, and the applications Zest
   recognises on your PC. Tick what you want cleaned.
2. Press **Analyze**. This only reads. For each rule you get a size and an item count.
3. Open a rule to see every file or entry behind it. Point at an item and press **Exclude** to
   leave that file or folder alone from now on (manage the list under
   *Options → Include and exclude*).
4. Press **Clean**, read the summary, confirm.

### What is ticked to begin with

- **On:** junk. Temporary files, caches, crash dumps, logs, thumbnails, the Recycle Bin.
- **Off:** anything personal. Cookies, browsing history, download history, saved form data, saved
  passwords, open sessions, recently used file lists. Turn these on only if you want that.

Temporary files newer than 24 hours are skipped, because a program may still be using them.
Change that under *Options → Cleaning*.

### Staying signed in

If you turn on cookie cleaning, add the sites you want to stay signed in to under
*Options → Cookies*. Zest suggests the common ones it finds on your PC. Cookies of kept sites are
left in the browser's database; everything else is removed.

### When a browser is open

A running browser locks its files. Zest marks those rules as blocked and names the program; close
the browser and analyze again.

## Afterwards

*Options → About* shows your clean history: when, how much, and from which areas.

Want this to happen by itself? See [Options and automation](Options-and-automation.md).
