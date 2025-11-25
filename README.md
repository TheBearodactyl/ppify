# ppify — osu! PP gains calculator

## Setup Guide

1. Open https://osu.ppy.sh/home/account/edit
2. Scroll down until you see the **OAuth** section.
3. Click **New OAuth Application**.
4. Fill in these fields:
   - **Application Name:** any name you like
   - **Application Callback URLs:** any valid URL (you can reuse your website or just `http://localhost`)
5. Click **Register application**.
6. On the application page, click **Show client secret**.
7. Create a new file named **.env** in the project folder. Add the following lines:

```sh
OSU_CLIENT_ID=<your Client ID number>
OSU_CLIENT_SECRET=<your Client Secret value>
```

8. Save the file, then run the app.
